//! Read-only native queries and connection-owned report supervision.
use super::{
    RemoteControlSession,
    device_model_values::{self as values, LearnedLimits},
    mapping,
};
use crate::command_registry::device_model201 as registry;
use crate::{OutboundCall, call::reports::ReportRoute};
use serde_json::Value;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::{sync::oneshot, time::Instant};
use uob_application::{CommandDispatchOutcome, DeviceModelStore201, RuntimeReservation, WorkClass};
use uob_contracts::{
    Command, CommandErrorCode, CommandLifecycle, CommandOperation, CommandResult, Connectivity,
    CorrelationId, DEVICE_MODEL_OUTPUT_LIMIT_201, DeviceModelResult201, DeviceReportAck201,
    DeviceReportFailure201, DeviceReportItem201, DeviceReportState201, DeviceVariableStatus201,
    Operation, ProtocolActionName, ProtocolEdition, RequestId, ResourceRef, StationSnapshot,
    UtcTimestamp,
};

pub(super) struct DeviceRuntime {
    pub store: Arc<dyn DeviceModelStore201>,
    pub generation: u64,
    pub limits: Arc<Mutex<LearnedLimits>>,
    active: AtomicBool,
}

struct PreparedDeviceCall {
    request: RequestId,
    resource: ResourceRef,
    pending: crate::PendingCall,
    route: Option<ReportRoute>,
    metadata: Arc<RuntimeReservation>,
    phase_sequence: u64,
}
impl RemoteControlSession {
    /// Host binds report persistence to this exact connection generation.
    #[must_use]
    pub fn with_device_model(
        mut self,
        store: Arc<dyn DeviceModelStore201>,
        generation: u64,
    ) -> Self {
        let actions = self
            .snapshot
            .read()
            .ok()
            .map(|snapshot| device_actions(&snapshot));
        let Some((enabled, reports)) = actions else {
            return self;
        };
        if !enabled {
            return self;
        }
        if reports {
            self.handle.enable_notify_reports();
        }
        self.device_model = Some(DeviceRuntime {
            store,
            generation,
            limits: Arc::new(Mutex::new(LearnedLimits::default())),
            active: AtomicBool::new(true),
        });
        self
    }
    /// Logical generation replacement/teardown closes reporting without changing grants.
    pub fn detach_device_model(&self) {
        if let Ok(mut phase) = self.phase.lock() {
            phase.detach();
        }
        if let Some(runtime) = &self.device_model {
            runtime.active.store(false, Ordering::Release);
        }
        self.handle.disconnect_reports();
    }
    pub(super) fn device_expectation(
        &self,
        command: &Command<Value>,
        now: UtcTimestamp,
    ) -> Result<Option<DeviceModelResult201>, CommandErrorCode> {
        let CommandOperation::Ocpp(operation) = &command.operation else {
            return Ok(None);
        };
        if operation.protocol != ProtocolEdition::Ocpp201
            || !registry::ACTIONS.contains(&operation.action.as_str())
        {
            return Ok(None);
        }
        let query = registry::validate(&command.resource, operation)
            .map_err(|_| CommandErrorCode::InvalidParameters)?;
        let runtime = self
            .device_model
            .as_ref()
            .ok_or(CommandErrorCode::UnsupportedOperation)?;
        if !runtime.active.load(Ordering::Acquire) {
            return Err(CommandErrorCode::StationDisconnected);
        }
        Ok(Some(DeviceModelResult201 {
            report: if query.request_id().is_some() {
                DeviceReportState201::Pending
            } else {
                DeviceReportState201::NotExpected
            },
            query,
            connection: self.handle.connection_id(),
            generation: runtime.generation,
            dispatch_recorded_at: now,
            native_ack: None,
            variables: Vec::new(),
        }))
    }
    pub(super) async fn dispatch_device(&self, command: Command<Value>) -> CommandDispatchOutcome {
        let request = command.request_id.clone();
        let outcome = self.dispatch_device_inner(command).await;
        if let CommandDispatchOutcome::NotTransmitted { error } = &outcome
            && let Some(runtime) = &self.device_model
            && let Ok(Some(result)) = runtime.store.device_model_result(request.clone()).await
            && let Some(evidence) = result.device_model_201
        {
            let lifecycle = CommandLifecycle::Rejected {
                error: error.clone(),
            };
            if runtime
                .store
                .finish_device_report(request, evidence, Some(lifecycle), self.clock.now())
                .await
                .is_err()
            {
                return mapping::uncertain();
            }
        }
        outcome
    }

    async fn dispatch_device_inner(&self, command: Command<Value>) -> CommandDispatchOutcome {
        let Some(runtime) = &self.device_model else {
            return mapping::not_sent(CommandErrorCode::UnsupportedOperation);
        };
        let Ok(Some(result)) = runtime
            .store
            .device_model_result(command.request_id.clone())
            .await
        else {
            return mapping::not_sent(CommandErrorCode::PolicyRejected);
        };
        let Some(evidence) = result.device_model_201 else {
            return mapping::not_sent(CommandErrorCode::PolicyRejected);
        };
        if !runtime.active.load(Ordering::Acquire)
            || evidence.connection != self.handle.connection_id()
            || evidence.generation != runtime.generation
        {
            return mapping::not_sent(CommandErrorCode::StationDisconnected);
        }
        let PreparedDeviceCall {
            request,
            resource,
            pending,
            route,
            metadata,
            phase_sequence,
        } = match self.prepare_device_call(command, &evidence, runtime) {
            Ok(prepared) => prepared,
            Err(error) => return mapping::not_sent(error),
        };
        let (stop, cancelled) = if route.is_some() {
            let (stop, cancelled) = oneshot::channel();
            (Some(stop), Some(cancelled))
        } else {
            (None, None)
        };
        let store = runtime.store.clone();
        let clock = self.clock.clone();
        let limits = runtime.limits.clone();
        if let Some(route) = route {
            self.spawn_device_collector(
                route,
                cancelled.expect("report cancellation receiver"),
                &evidence,
                request.clone(),
                metadata.clone(),
                runtime,
            );
        }
        // The native reply writer also survives origin cancellation. No HTTP future owns routes.
        let (response, received) = oneshot::channel();
        let writer = super::device_model_response::ReplyWriter201 {
            store,
            clock,
            limits,
            budget: self.handle.resource_budget(),
            request,
            resource,
            evidence,
            stop,
            _metadata: metadata,
            handle: self.handle.clone(),
            phase: self.phase.clone(),
            phase_sequence,
        };
        tokio::spawn(async move {
            let _ = response.send(writer.receive(pending).await);
        });
        received.await.unwrap_or_else(|_| mapping::uncertain())
    }

    fn prepare_device_call(
        &self,
        command: Command<Value>,
        evidence: &DeviceModelResult201,
        runtime: &DeviceRuntime,
    ) -> Result<PreparedDeviceCall, CommandErrorCode> {
        let now = self.validate_device_dispatch(&command, evidence, runtime)?;
        let metadata = self.reserve_device_metadata(evidence)?;
        let remaining = (command.expires_at.into_inner() - now.into_inner()).unsigned_abs();
        let phase_sequence = self
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin(&evidence.query);
        let deadline = Instant::now() + remaining.min(std::time::Duration::from_hours(24));
        let CommandOperation::Ocpp(operation) = command.operation else {
            return Err(CommandErrorCode::InvalidParameters);
        };
        let call = OutboundCall {
            message_id: command.request_id.as_str().to_owned(),
            action: operation.action,
            payload: operation.payload,
            correlation_id: command.correlation_id.clone().unwrap_or_else(|| {
                CorrelationId::new(command.request_id.as_str()).expect("request identity")
            }),
        };
        let (pending, route) = if let Some(request_id) = evidence.query.request_id() {
            let key = crate::multipart::ReportKey {
                station: command.resource.station_id.clone(),
                connection: evidence.connection.clone(),
                protocol: ProtocolEdition::Ocpp201,
                action: ProtocolActionName::new("NotifyReport").expect("static action"),
                request_id,
                correlation: call.correlation_id.clone(),
            };
            let (pending, route) = self
                .handle
                .try_report_call_before(call, deadline, key, command.resource.clone())
                .map_err(|_| CommandErrorCode::PolicyRejected)?;
            (pending, Some(route))
        } else {
            let pending = self
                .handle
                .try_call_before(call, deadline)
                .map_err(|_| CommandErrorCode::PolicyRejected)?;
            (pending, None)
        };
        Ok(PreparedDeviceCall {
            request: command.request_id,
            resource: command.resource,
            pending,
            route,
            metadata,
            phase_sequence,
        })
    }

    fn validate_device_dispatch(
        &self,
        command: &Command<Value>,
        evidence: &DeviceModelResult201,
        runtime: &DeviceRuntime,
    ) -> Result<UtcTimestamp, CommandErrorCode> {
        let CommandOperation::Ocpp(operation) = &command.operation else {
            return Err(CommandErrorCode::InvalidParameters);
        };
        if registry::validate(&command.resource, operation)
            .ok()
            .as_ref()
            != Some(&evidence.query)
        {
            return Err(CommandErrorCode::InvalidParameters);
        }
        let now = self.clock.now();
        {
            let Ok(snapshot) = self.snapshot.read() else {
                return Err(CommandErrorCode::PolicyRejected);
            };
            if self.handle.is_closed()
                || !matches!(snapshot.connectivity, Connectivity::Connected { connected_at, .. } if command.admitted_at >= connected_at)
            {
                return Err(CommandErrorCode::StationDisconnected);
            }
            if uob_application::registration::v201::accepted(&snapshot).is_err()
                || mapping::capabilities(&snapshot, &command.resource)
                    .is_none_or(|caps| command.validate_for_dispatch(caps, now).is_err())
            {
                return Err(CommandErrorCode::PolicyRejected);
            }
        }
        let Some(bytes) = super::device_model_collection::json_size(
            &(
                2,
                command.request_id.as_str(),
                operation.action.as_str(),
                &operation.payload,
            ),
            256 * 1024,
        ) else {
            return Err(CommandErrorCode::InvalidParameters);
        };
        if !runtime
            .limits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .allows(&evidence.query, bytes)
        {
            return Err(CommandErrorCode::PolicyRejected);
        }
        Ok(now)
    }

    fn reserve_device_metadata(
        &self,
        evidence: &DeviceModelResult201,
    ) -> Result<Arc<RuntimeReservation>, CommandErrorCode> {
        let Some(size) =
            super::device_model_collection::json_size(evidence, DEVICE_MODEL_OUTPUT_LIMIT_201)
        else {
            return Err(CommandErrorCode::InvalidParameters);
        };
        let Ok(reserved) = self
            .handle
            .resource_budget()
            .try_reserve(WorkClass::PendingRequest, size * 20 + 4096)
        else {
            return Err(CommandErrorCode::PolicyRejected);
        };
        Ok(Arc::new(reserved))
    }

    fn spawn_device_collector(
        &self,
        route: ReportRoute,
        cancelled: oneshot::Receiver<()>,
        evidence: &DeviceModelResult201,
        request: RequestId,
        metadata: Arc<RuntimeReservation>,
        runtime: &DeviceRuntime,
    ) {
        let budget = self.handle.resource_budget();
        let query = evidence.clone();
        let mut fallback = query.clone();
        let handle = self.handle.clone();
        let store = runtime.store.clone();
        let clock = self.clock.clone();
        let learned = runtime.limits.clone();
        tokio::spawn(async move {
            let report = super::device_model_collection::collect(route, budget, cancelled).await;
            let mut query = query;
            query.report = report.report;
            let progress = match &query.report {
                DeviceReportState201::Complete { progress, .. } => Some(*progress),
                DeviceReportState201::Incomplete { progress, .. } => *progress,
                _ => None,
            };
            if let Ok(Some(result)) = store
                .finish_device_report(request.clone(), query, None, clock.now())
                .await
            {
                learn_durable(&learned, &result);
            } else {
                handle.report_store_failed(fallback.connection.clone());
                fallback.report = DeviceReportState201::Incomplete {
                    reason: DeviceReportFailure201::StorageUnavailable,
                    progress,
                };
                if store
                    .finish_device_report(request, fallback, None, clock.now())
                    .await
                    .is_err()
                {
                    handle.report_store_failed(handle.connection_id());
                }
            }
            drop(report.retained);
            drop(metadata);
        });
    }
}

pub(crate) struct SanitizedNotification {
    pub sequence: u32,
    pub more: bool,
    pub generated_at: String,
    pub items: Vec<DeviceReportItem201>,
}
pub(crate) fn notification(
    payload: &Value,
    resource: &ResourceRef,
) -> Option<SanitizedNotification> {
    if payload["generatedAt"].as_str()?.len() > 128 {
        return None;
    }
    Some(SanitizedNotification {
        sequence: u32::try_from(payload["seqNo"].as_u64()?).ok()?,
        more: payload.get("tbc").and_then(Value::as_bool).unwrap_or(false),
        generated_at: payload["generatedAt"].as_str()?.to_owned(),
        items: values::report_items(payload, resource)?,
    })
}

pub(super) fn learn_durable(learned: &Mutex<LearnedLimits>, result: &CommandResult) {
    let Some(evidence) = &result.device_model_201 else {
        return;
    };
    let mut learned = learned
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for item in &evidence.variables {
        if item.status == DeviceVariableStatus201::Accepted {
            learned.learn(
                &item.component,
                &item.variable,
                item.attribute_type,
                &item.value,
            );
        }
    }
    if evidence.native_ack == Some(DeviceReportAck201::Accepted)
        && let DeviceReportState201::Complete { items, .. } = &evidence.report
    {
        for item in items {
            for attribute in &item.attributes {
                learned.learn(
                    &item.component,
                    &item.variable,
                    attribute.attribute_type,
                    &attribute.value,
                );
            }
        }
    }
}

fn device_actions(snapshot: &StationSnapshot) -> (bool, bool) {
    let mut enabled = false;
    let mut reports = false;
    for caps in std::iter::once(&snapshot.capabilities)
        .chain(snapshot.resources.iter().map(|entry| &entry.capabilities))
    {
        for supported in &caps.operations {
            if let Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp201,
                action,
            } = &supported.operation
                && registry::ACTIONS.contains(&action.as_str())
            {
                enabled = true;
                reports |= action != "GetVariables";
            }
        }
    }
    (enabled, reports)
}
