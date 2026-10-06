//! One connection's shared native report namespaces; no independent socket reader.
//! `NotifyReport` and `ReportChargingProfiles` request IDs are separate native namespaces that
//! share the bounded route slots, retired-ID capacity and the socket owner's budget.
mod session;
use crate::multipart::{ReportFailure, ReportFragment, ReportKey};
use crate::v201::remote_control::device_model::{SanitizedNotification, notification};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use uob_application::{RuntimeReservation, RuntimeResourceBudget, WorkClass};
use uob_contracts::{
    ChargingProfileReportFragment201, ChargingProfilesQuery201, DeviceReportFragment201,
    ResourceRef,
};

/// Native charger-to-CSMS report action routed by the socket owner.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum ReportKind {
    Device,
    ChargingProfiles,
}
impl ReportKind {
    pub(crate) fn from_action(action: &str) -> Option<Self> {
        match action {
            "NotifyReport" => Some(Self::Device),
            "ReportChargingProfiles" => Some(Self::ChargingProfiles),
            _ => None,
        }
    }
    const fn index(self) -> usize {
        match self {
            Self::Device => 0,
            Self::ChargingProfiles => 1,
        }
    }
}
/// Immutable request context a fragment must match before it is accepted.
pub(crate) enum RouteContext {
    Device,
    ChargingProfiles(ChargingProfilesQuery201),
}
impl RouteContext {
    const fn kind(&self) -> ReportKind {
        match self {
            Self::Device => ReportKind::Device,
            Self::ChargingProfiles(_) => ReportKind::ChargingProfiles,
        }
    }
}

pub(crate) struct Registry {
    routes: BTreeMap<(ReportKind, i32), Entry>,
    used: HashSet<(ReportKind, i32)>,
    enabled: [bool; 2],
    closed: bool,
    budget: RuntimeResourceBudget,
    reservation: Option<RuntimeReservation>,
}
struct Entry {
    key: ReportKey,
    resource: ResourceRef,
    context: RouteContext,
    /// Arrival position for actions without a native sequence number.
    next_sequence: u32,
    sender: mpsc::Sender<Ingress>,
    failure: watch::Sender<Option<ReportFailure>>,
}
pub(crate) type SharedReports = Arc<Mutex<Registry>>;
pub(crate) enum FragmentMetadata {
    Device(DeviceReportFragment201),
    ChargingProfiles(ChargingProfileReportFragment201),
}
pub(crate) struct Ingress {
    pub fragment: ReportFragment,
    pub metadata: FragmentMetadata,
    pub(crate) reservation: RuntimeReservation,
}
/// A registered route unregisters on every collector exit. IDs remain retired until teardown.
pub(crate) struct ReportRoute {
    registry: SharedReports,
    request: (ReportKind, i32),
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
        enabled: [false; 2],
        closed: false,
        budget: budget.clone(),
        reservation: None,
    }))
}
pub(crate) fn register(
    registry: &SharedReports,
    key: ReportKey,
    resource: ResourceRef,
    context: RouteContext,
    admission: crate::multipart::CollectedReport,
) -> Result<(ReportRoute, oneshot::Sender<Instant>), crate::SessionSubmitError> {
    let kind = context.kind();
    if ReportKind::from_action(key.action.as_str()) != Some(kind) {
        return Err(crate::SessionSubmitError::InvalidRequest);
    }
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
    let request = (kind, key.request_id);
    if current.used.len() == 4096 || current.used.contains(&request) || current.routes.len() == 4 {
        return Err(crate::SessionSubmitError::Full);
    }
    current.enabled[kind.index()] = true;
    let (sender, receiver) = mpsc::channel(4);
    let (failure, failed) = watch::channel(None);
    let (started, start) = oneshot::channel();
    current.used.insert(request);
    current.routes.insert(
        request,
        Entry {
            key,
            resource,
            context,
            next_sequence: 0,
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
    current.enabled = [false; 2];
}
pub(crate) fn enable(registry: &SharedReports, kind: ReportKind) {
    let mut current = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !current.closed {
        current.enabled[kind.index()] = true;
    }
}

/// Returns None when default-off; wire acknowledgement is never durable completion.
pub(crate) fn route(
    registry: &SharedReports,
    kind: ReportKind,
    payload: &serde_json::Value,
    bytes: usize,
    budget: &RuntimeResourceBudget,
) -> Option<bool> {
    if !registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .enabled[kind.index()]
    {
        return None;
    }
    if kind == ReportKind::ChargingProfiles {
        return Some(route_profiles(registry, payload, bytes, budget));
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
    let Some(entry) = request.and_then(|request| current.routes.get_mut(&(kind, request))) else {
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
        metadata: FragmentMetadata::Device(DeviceReportFragment201 {
            generated_at,
            sequence,
            more,
            items: count,
        }),
        reservation,
    };
    enqueue_fragment(entry, ingress);
    Some(true)
}

/// Fragments carry no sequence number; the socket owner's arrival order assigns one.
fn route_profiles(
    registry: &SharedReports,
    payload: &serde_json::Value,
    bytes: usize,
    budget: &RuntimeResourceBudget,
) -> bool {
    use crate::command_registry::charging_profiles201 as profiles;
    let valid = profiles::valid_schema(profiles::REPORT, payload);
    let request = payload["requestId"]
        .as_i64()
        .and_then(|id| i32::try_from(id).ok());
    let mut current = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(entry) = request.and_then(|request| {
        current
            .routes
            .get_mut(&(ReportKind::ChargingProfiles, request))
    }) else {
        return valid;
    };
    if !valid {
        entry
            .failure
            .send_replace(Some(ReportFailure::InvalidFragment));
        return false;
    }
    let RouteContext::ChargingProfiles(query) = &entry.context else {
        entry
            .failure
            .send_replace(Some(ReportFailure::CorrelationMismatch));
        return true;
    };
    let reservation = match budget.try_reserve(
        WorkClass::PendingRequest,
        bytes.saturating_mul(8).saturating_add(4096),
    ) {
        Ok(reservation) => reservation,
        Err(error) => {
            entry
                .failure
                .send_replace(Some(ReportFailure::Capacity(error)));
            return true;
        }
    };
    let fragment = match crate::v201::remote_control::charging_profiles_report::fragment(
        payload,
        query,
        entry.next_sequence,
    ) {
        Ok(fragment) => fragment,
        Err(reason) => {
            entry.failure.send_replace(Some(reason));
            return reason != ReportFailure::InvalidFragment;
        }
    };
    let sequence = entry.next_sequence;
    entry.next_sequence = entry.next_sequence.saturating_add(1);
    let ingress = Ingress {
        fragment: ReportFragment {
            key: entry.key.clone(),
            sequence,
            more: fragment.metadata.more,
            items: fragment.items,
        },
        metadata: FragmentMetadata::ChargingProfiles(fragment.metadata),
        reservation,
    };
    enqueue_fragment(entry, ingress);
    true
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
