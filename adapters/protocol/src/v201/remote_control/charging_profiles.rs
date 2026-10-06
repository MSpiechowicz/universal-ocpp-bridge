//! Native K09 installed-profile queries. One task owns the exchange: it records the native
//! acknowledgement durably before releasing the command outcome, then the collected report.
//! Neither write depends on the origin's response future, and nothing is replayed.
use super::{
    RemoteControlSession,
    charging_profiles_report::{CollectedProfiles201, collect},
    composite_schedule::reason,
    mapping,
};
use crate::command_registry::charging_profiles201 as registry;
use crate::{
    CallSessionHandle, OutboundCall, PendingCall, SessionCallOutcome, SessionSubmitError,
    call::reports::{ReportRoute, RouteContext},
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use tokio::{sync::oneshot, time::Instant};
use uob_application::{
    ChargingProfileReportStore201, CommandClock, CommandDispatchOutcome, RuntimeResourceBudget,
};
use uob_contracts::{
    ChargingProfileReportFailure201, ChargingProfileReportState201, ChargingProfilesQuery201,
    ChargingProfilesResult201, ChargingProfilesStatus201, Command, CommandErrorCode,
    CommandLifecycle, CommandOperation, Connectivity, CorrelationId, Operation, ProtocolActionName,
    ProtocolEdition, RequestId, StationSnapshot,
};

fn advertised(snapshot: &StationSnapshot) -> bool {
    let operation = Operation::ProtocolAction {
        protocol: ProtocolEdition::Ocpp201,
        action: registry::ACTION.to_owned(),
    };
    std::iter::once(&snapshot.capabilities)
        .chain(snapshot.resources.iter().map(|entry| &entry.capabilities))
        .any(|capabilities| capabilities.supports(&operation))
}

impl RemoteControlSession {
    /// Host binds durable report completion; without it the action stays unsupported.
    #[must_use]
    pub fn with_charging_profile_reports(
        mut self,
        store: Arc<dyn ChargingProfileReportStore201>,
    ) -> Self {
        if self.snapshot.read().is_ok_and(|s| advertised(&s)) {
            self.handle.enable_profile_reports();
            self.profile_reports = Some(store);
        }
        self
    }

    pub(super) async fn dispatch_charging_profiles(
        &self,
        command: Command<Value>,
    ) -> CommandDispatchOutcome {
        let Some(store) = self.profile_reports.clone() else {
            return mapping::not_sent(CommandErrorCode::UnsupportedOperation);
        };
        let (query, pending, route) = match self.prepare_charging_profiles(&command) {
            Ok(prepared) => prepared,
            Err(code) => return mapping::not_sent(code),
        };
        let exchange = Exchange {
            store,
            clock: self.clock.clone(),
            budget: self.handle.resource_budget(),
            handle: self.handle.clone(),
            request: command.request_id,
            query,
        };
        let (outcome, received) = oneshot::channel();
        tokio::spawn(exchange.run(pending, route, outcome));
        received.await.unwrap_or_else(|_| mapping::uncertain())
    }

    fn prepare_charging_profiles(
        &self,
        command: &Command<Value>,
    ) -> Result<(ChargingProfilesQuery201, PendingCall, ReportRoute), CommandErrorCode> {
        use CommandErrorCode::{Expired, InvalidParameters, PolicyRejected, StationDisconnected};
        let CommandOperation::Ocpp(operation) = &command.operation else {
            return Err(InvalidParameters);
        };
        let query = registry::validate(&command.resource, operation)?;
        let now = self.clock.now();
        {
            let snapshot = self.snapshot.read().map_err(|_| PolicyRejected)?;
            if self.handle.is_closed()
                || !matches!(snapshot.connectivity, Connectivity::Connected { connected_at, .. }
                    if command.admitted_at >= connected_at)
            {
                return Err(StationDisconnected);
            }
            uob_application::registration::v201::accepted(&snapshot).map_err(|_| PolicyRejected)?;
            let capabilities =
                mapping::capabilities(&snapshot, &command.resource).ok_or(InvalidParameters)?;
            command
                .validate_for_dispatch(capabilities, now)
                .map_err(|error| match error {
                    uob_contracts::CommandValidationError::Expired => Expired,
                    uob_contracts::CommandValidationError::UnsupportedOperation(_) => {
                        PolicyRejected
                    }
                })?;
        }
        let remaining = (command.expires_at.into_inner() - now.into_inner()).unsigned_abs();
        let deadline = Instant::now() + remaining.min(std::time::Duration::from_hours(24));
        let call = OutboundCall {
            message_id: command.request_id.as_str().to_owned(),
            action: operation.action.clone(),
            payload: operation.payload.clone(),
            correlation_id: command.correlation_id.clone().unwrap_or_else(|| {
                CorrelationId::new(command.request_id.as_str()).expect("request identity")
            }),
        };
        let key = crate::multipart::ReportKey {
            station: command.resource.station_id.clone(),
            connection: self.handle.connection_id(),
            protocol: ProtocolEdition::Ocpp201,
            action: ProtocolActionName::new(registry::REPORT_ACTION).expect("static action"),
            request_id: query.request_id,
            correlation: call.correlation_id.clone(),
        };
        let (pending, route) = self
            .handle
            .try_report_call_before(
                call,
                deadline,
                key,
                command.resource.clone(),
                RouteContext::ChargingProfiles(query.clone()),
            )
            .map_err(|error| match error {
                SessionSubmitError::Closed => StationDisconnected,
                SessionSubmitError::InvalidRequest => InvalidParameters,
                SessionSubmitError::Resource(_) | SessionSubmitError::Full => PolicyRejected,
            })?;
        Ok((query, pending, route))
    }
}

struct Exchange {
    store: Arc<dyn ChargingProfileReportStore201>,
    clock: Arc<dyn CommandClock>,
    budget: RuntimeResourceBudget,
    handle: CallSessionHandle,
    request: RequestId,
    query: ChargingProfilesQuery201,
}

impl Exchange {
    async fn run(
        self,
        pending: PendingCall,
        route: ReportRoute,
        outcome: oneshot::Sender<CommandDispatchOutcome>,
    ) {
        // Poll collection beside the reply so fragments queued behind it never overflow the route.
        let collection = collect(route, self.budget.clone());
        let reply = pending.receive();
        tokio::pin!(collection, reply);
        let mut early = None;
        let reply = tokio::select! {
            biased;
            reply = &mut reply => reply,
            report = &mut collection => {
                early = Some(report);
                reply.await
            }
        };
        let mut evidence = match reply {
            SessionCallOutcome::Result { payload, .. } => match self.acknowledgement(&payload) {
                Some(evidence) => evidence,
                None => return drop(outcome.send(mapping::uncertain())),
            },
            SessionCallOutcome::Error { .. } => {
                return drop(outcome.send(mapping::rejected_response()));
            }
            SessionCallOutcome::NotTransmitted { reason, .. } => {
                let code = if reason == "command expired before socket send" {
                    CommandErrorCode::Expired
                } else {
                    CommandErrorCode::PolicyRejected
                };
                return drop(outcome.send(mapping::not_sent(code)));
            }
            SessionCallOutcome::TimedOut { .. }
            | SessionCallOutcome::TransmissionUncertain { .. } => {
                return drop(outcome.send(mapping::uncertain()));
            }
        };
        // A finished collection future is never polled again, whatever the reply said.
        let collected_early = early.is_some();
        let mut retained = None;
        if evidence.report.pending()
            && let Some(CollectedProfiles201 {
                report,
                retained: kept,
            }) = early.take()
        {
            evidence.report = report;
            retained = kept;
        }
        let lifecycle = CommandLifecycle::ProtocolResponse {
            accepted: evidence.status == ChargingProfilesStatus201::Accepted,
            error: None,
        };
        let recorded = self.finish(evidence, Some(lifecycle)).await;
        let Some(recorded) = recorded else {
            return drop(outcome.send(mapping::uncertain()));
        };
        let collecting = recorded.report.pending() && !collected_early;
        drop(
            outcome.send(CommandDispatchOutcome::ChargingProfilesResponse201(
                recorded.clone(),
            )),
        );
        drop(retained);
        if !collecting {
            return;
        }
        let CollectedProfiles201 { report, retained } = collection.await;
        let mut completed = recorded;
        completed.report = report;
        if self.finish(completed.clone(), None).await.is_none() {
            completed.report = ChargingProfileReportState201::Incomplete {
                reason: ChargingProfileReportFailure201::StorageUnavailable,
                progress: None,
            };
            let _ = self.finish(completed, None).await;
        }
        drop(retained);
    }

    /// The pinned schema bounds `statusInfo`; only listed reason codes are retained.
    fn acknowledgement(&self, payload: &Value) -> Option<ChargingProfilesResult201> {
        if !registry::valid_schema(registry::RESPONSE, payload) {
            return None;
        }
        let status = ChargingProfilesStatus201::deserialize(&payload["status"]).ok()?;
        Some(ChargingProfilesResult201 {
            query: self.query.clone(),
            status,
            reason_code: reason(payload),
            report: match status {
                ChargingProfilesStatus201::Accepted => ChargingProfileReportState201::Pending,
                // No report follows NoProfiles; late fragments are acknowledged but not retained.
                ChargingProfilesStatus201::NoProfiles => ChargingProfileReportState201::NotExpected,
            },
        })
    }

    async fn finish(
        &self,
        evidence: ChargingProfilesResult201,
        lifecycle: Option<CommandLifecycle>,
    ) -> Option<ChargingProfilesResult201> {
        let stored = self
            .store
            .finish_charging_profiles(self.request.clone(), evidence, lifecycle, self.clock.now())
            .await;
        let recorded = stored
            .ok()
            .flatten()
            .and_then(|result| result.charging_profiles_201);
        if recorded.is_none() {
            self.handle.report_store_failed(self.handle.connection_id());
        }
        recorded
    }
}
