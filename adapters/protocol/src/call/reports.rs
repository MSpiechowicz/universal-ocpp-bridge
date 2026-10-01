//! One connection's shared `NotifyReport` namespace; no independent socket reader.
use super::types::{CallSessionHandle, OutboundCall, PendingCall, SessionSubmitError};
use crate::multipart::{ReportFailure, ReportFragment, ReportKey};
use crate::v201::remote_control::device_model::{SanitizedNotification, notification};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use uob_application::{RuntimeReservation, RuntimeResourceBudget, WorkClass};
use uob_contracts::CorrelationId;
use uob_contracts::{DeviceReportFragment201, ResourceRef};

pub(crate) struct Registry {
    routes: BTreeMap<i32, Entry>,
    used: HashSet<i32>,
    enabled: bool,
    closed: bool,
    budget: RuntimeResourceBudget,
    reservation: Option<RuntimeReservation>,
}
struct Entry {
    key: ReportKey,
    resource: ResourceRef,
    sender: mpsc::Sender<Ingress>,
    failure: watch::Sender<Option<ReportFailure>>,
}
pub(crate) type SharedReports = Arc<Mutex<Registry>>;
pub(crate) struct Ingress {
    pub fragment: ReportFragment,
    pub metadata: DeviceReportFragment201,
    pub(crate) reservation: RuntimeReservation,
}
/// A registered route unregisters on every collector exit. IDs remain retired until teardown.
pub(crate) struct ReportRoute {
    registry: SharedReports,
    request: i32,
    admission: Option<crate::multipart::CollectedReport>,
    receiver: mpsc::Receiver<Ingress>,
    failure: watch::Receiver<Option<ReportFailure>>,
    start: oneshot::Receiver<Instant>,
}
impl Drop for ReportRoute {
    fn drop(&mut self) {
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .routes
            .remove(&self.request);
    }
}
impl ReportRoute {
    pub(crate) fn take_admission(&mut self) -> crate::multipart::CollectedReport {
        self.admission
            .take()
            .expect("one report collector per route")
    }
    pub async fn dispatch_started(&mut self) -> Result<Instant, ReportFailure> {
        (&mut self.start)
            .await
            .map_err(|_| ReportFailure::NotTransmitted)
    }
    pub async fn next(&mut self) -> Result<Option<Ingress>, ReportFailure> {
        if let Some(reason) = *self.failure.borrow() {
            return Err(reason);
        }
        tokio::select! {
            biased;
            change = self.failure.changed() => {
                if change.is_err() { return Ok(None); }
                Err(self.failure.borrow().unwrap_or(ReportFailure::Disconnected))
            }
            item = self.receiver.recv() => Ok(item),
        }
    }
}
pub(crate) fn registry(budget: &RuntimeResourceBudget) -> SharedReports {
    Arc::new(Mutex::new(Registry {
        routes: BTreeMap::new(),
        used: HashSet::new(),
        enabled: false,
        closed: false,
        budget: budget.clone(),
        reservation: None,
    }))
}
pub(crate) fn register(
    registry: &SharedReports,
    key: ReportKey,
    resource: ResourceRef,
    admission: crate::multipart::CollectedReport,
) -> Result<(ReportRoute, oneshot::Sender<Instant>), crate::SessionSubmitError> {
    let mut current = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if current.closed {
        return Err(crate::SessionSubmitError::Closed);
    }
    if current.reservation.is_none() {
        let reservation = current
            .budget
            .try_reserve(WorkClass::PendingRequest, 4096 * 32 + 8192)
            .map_err(crate::SessionSubmitError::Resource)?;
        current.used = HashSet::with_capacity(4096);
        current.reservation = Some(reservation);
    }
    if current.used.len() == 4096
        || current.used.contains(&key.request_id)
        || current.routes.len() == 4
    {
        return Err(crate::SessionSubmitError::Full);
    }
    current.enabled = true;
    let request = key.request_id;
    let (sender, receiver) = mpsc::channel(4);
    let (failure, failed) = watch::channel(None);
    let (started, start) = oneshot::channel();
    current.used.insert(request);
    current.routes.insert(
        request,
        Entry {
            key,
            resource,
            sender,
            failure,
        },
    );
    Ok((
        ReportRoute {
            registry: registry.clone(),
            request,
            admission: Some(admission),
            receiver,
            failure: failed,
            start,
        },
        started,
    ))
}
pub(crate) fn disconnect(registry: &SharedReports) {
    let mut current = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (_, entry) in std::mem::take(&mut current.routes) {
        entry
            .failure
            .send_replace(Some(ReportFailure::Disconnected));
    }
    current.used = HashSet::new();
    current.reservation = None;
    current.closed = true;
    current.enabled = false;
}
pub(crate) fn enable(registry: &SharedReports) {
    let mut current = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !current.closed {
        current.enabled = true;
    }
}

/// Returns None when default-off; wire acknowledgement is never durable completion.
pub(crate) fn route(
    registry: &SharedReports,
    payload: &serde_json::Value,
    bytes: usize,
    budget: &RuntimeResourceBudget,
) -> Option<bool> {
    if !registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .enabled
    {
        return None;
    }
    let valid = crate::command_registry::device_model201::valid_schema(6, payload)
        && payload["requestId"]
            .as_i64()
            .is_some_and(|id| i32::try_from(id).is_ok())
        && payload["seqNo"]
            .as_u64()
            .is_some_and(|seq| u32::try_from(seq).is_ok());
    let request = payload["requestId"]
        .as_i64()
        .and_then(|id| i32::try_from(id).ok());
    let mut current = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(entry) = request.and_then(|request| current.routes.get_mut(&request)) else {
        return Some(valid);
    };
    if !valid {
        entry
            .failure
            .send_replace(Some(ReportFailure::InvalidFragment));
        return Some(false);
    }
    // Reserve the worst-case escaped expansion and typed descriptors before sanitizing/copying.
    let reservation = match budget.try_reserve(
        WorkClass::PendingRequest,
        bytes.saturating_mul(8).saturating_add(4096),
    ) {
        Ok(reservation) => reservation,
        Err(error) => {
            entry
                .failure
                .send_replace(Some(ReportFailure::Capacity(error)));
            return Some(true);
        }
    };
    if !crate::v201::remote_control::device_model_values::report_scope_matches(
        payload,
        &entry.resource,
    ) {
        entry
            .failure
            .send_replace(Some(ReportFailure::CorrelationMismatch));
        return Some(true);
    }
    let Some(SanitizedNotification {
        sequence,
        more,
        generated_at,
        items,
    }) = notification(payload, &entry.resource)
    else {
        entry
            .failure
            .send_replace(Some(ReportFailure::InvalidFragment));
        return Some(false);
    };
    let count = items.len();
    let items = items
        .into_iter()
        .map(|item| serde_json::to_vec(&item))
        .collect::<Result<Vec<_>, _>>();
    let Ok(items) = items else {
        entry.failure.send_replace(Some(ReportFailure::ByteLimit));
        return Some(true);
    };
    let ingress = Ingress {
        fragment: ReportFragment {
            key: entry.key.clone(),
            sequence,
            more,
            items,
        },
        metadata: DeviceReportFragment201 {
            generated_at,
            sequence,
            more,
            items: count,
        },
        reservation,
    };
    enqueue_fragment(entry, ingress);
    Some(true)
}

fn enqueue_fragment(entry: &Entry, ingress: Ingress) {
    match entry.sender.try_send(ingress) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            entry.failure.send_replace(Some(ReportFailure::Capacity(
                uob_application::AdmissionError {
                    limit: uob_application::AdmissionLimit::QueueItems(WorkClass::PendingRequest),
                    maximum: 4,
                    requested: 5,
                },
            )));
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            entry
                .failure
                .send_replace(Some(ReportFailure::Disconnected));
        }
    }
}

impl CallSessionHandle {
    pub(crate) fn enable_notify_reports(&self) {
        enable(&self.reports);
    }
    pub(crate) fn report_store_failed(&self, correlation: CorrelationId) {
        self.diagnostics
            .span(Some(correlation), Some(self.station_id.clone()), None)
            .emit(
                uob_application::FlowStage::DurableCommit,
                uob_application::FlowEvidence::Failed,
            );
    }
    pub(crate) fn disconnect_reports(&self) {
        disconnect(&self.reports);
    }
    /// Registers the shared native report route before enqueuing its CALL.
    pub(crate) fn try_report_call_before(
        &self,
        request: OutboundCall,
        deadline: Instant,
        key: crate::multipart::ReportKey,
        resource: uob_contracts::ResourceRef,
    ) -> Result<(PendingCall, super::reports::ReportRoute), SessionSubmitError> {
        if self.is_closed() {
            return Err(SessionSubmitError::Closed);
        }
        if key.station != self.station_id
            || resource.station_id != self.station_id
            || key.connection != self.connection_id
            || key.protocol != uob_contracts::ProtocolEdition::Ocpp201
            || self.protocol != key.protocol
            || key.action.as_str() != "NotifyReport"
            || key.correlation != request.correlation_id
            || !["GetBaseReport", "GetReport"].contains(&request.action.as_str())
            || request.payload["requestId"].as_i64() != Some(i64::from(key.request_id))
        {
            return Err(SessionSubmitError::InvalidRequest);
        }
        let admission = crate::multipart::reserve_report(
            key.clone(),
            crate::multipart::ReportLimits::default(),
            &self.budget,
        )
        .map_err(|_| SessionSubmitError::Full)?;
        let (route, started) = super::reports::register(&self.reports, key, resource, admission)?;
        let pending = self.enqueue_observed(request, Some(deadline), None, Some(started))?;
        Ok((pending, route))
    }

    /// Budget used by the single socket owner, shared with report collection.
    pub(crate) fn resource_budget(&self) -> uob_application::RuntimeResourceBudget {
        self.budget.clone()
    }
    /// Opaque identity of the authenticated connection, never shared across reconnects.
    pub(crate) fn connection_id(&self) -> CorrelationId {
        self.connection_id.clone()
    }
}

pub(crate) fn capacity_failure(registry: &SharedReports, error: uob_application::AdmissionError) {
    let current = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for entry in current.routes.values() {
        entry
            .failure
            .send_replace(Some(ReportFailure::Capacity(error)));
    }
}

#[cfg(test)]
mod tests;
