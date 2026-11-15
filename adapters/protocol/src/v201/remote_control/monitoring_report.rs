//! `GetMonitoringReport` (N02). One task owns the exchange: it matches the native acknowledgement
//! with the fragments that follow it, releases the command outcome with the acknowledgement, and
//! completes the report through the durable store. The origin's response future owns neither, and
//! nothing is replayed.
use super::{
    RemoteControlSession,
    device_model_collection::json_size,
    mapping,
    monitoring::{Prepared, submit_error},
    monitoring_collect::{Collected, collect},
    monitoring_reply::report_ack,
};
use crate::{
    PendingCall, SessionCallOutcome,
    call::reports::{ReportRoute, RouteContext},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;
use uob_application::{
    CommandClock, CommandDispatchOutcome, MonitoringReportStore201, StorageError,
    monitoring_response_lifecycle,
};
use uob_contracts::{
    Command, CommandErrorCode, CommandResult, DeviceReportAck201, DeviceReportFailure201,
    MONITORING_REPORT_OUTPUT_LIMIT_201, MonitoringOutcome201, MonitoringQuery201,
    MonitoringReportState201, MonitoringResult201, ProtocolActionName, ProtocolEdition, RequestId,
};

impl RemoteControlSession {
    pub(super) async fn dispatch_monitoring_report(
        &self,
        command: &Command<serde_json::Value>,
        prepared: Prepared,
    ) -> CommandDispatchOutcome {
        let Some(store) = self.monitoring_reports.clone() else {
            return mapping::not_sent(CommandErrorCode::UnsupportedOperation);
        };
        let Prepared {
            query,
            call,
            deadline,
        } = prepared;
        let Some(request_id) = query.request_id() else {
            return mapping::not_sent(CommandErrorCode::InvalidParameters);
        };
        let key = crate::multipart::ReportKey {
            station: command.resource.station_id.clone(),
            connection: self.handle.connection_id(),
            protocol: ProtocolEdition::Ocpp201,
            action: ProtocolActionName::new("NotifyMonitoringReport").expect("static action"),
            request_id,
            correlation: call.correlation_id.clone(),
        };
        let (pending, route) = match self.handle.try_report_call_before(
            call,
            deadline,
            key,
            command.resource.clone(),
            RouteContext::Monitoring(query.clone()),
        ) {
            Ok(registered) => registered,
            Err(error) => return mapping::not_sent(submit_error(error)),
        };

        let exchange = Exchange {
            store,
            clock: self.clock.clone(),
            budget: self.handle.resource_budget(),
            handle: self.handle.clone(),
            request: command.request_id.clone(),
            query,
        };
        let (outcome, received) = oneshot::channel();
        tokio::spawn(exchange.run(pending, route, outcome));

        received.await.unwrap_or_else(|_| mapping::uncertain())
    }
}

struct Exchange {
    store: Arc<dyn MonitoringReportStore201>,
    clock: Arc<dyn CommandClock>,
    budget: uob_application::RuntimeResourceBudget,
    handle: crate::CallSessionHandle,
    request: RequestId,
    query: MonitoringQuery201,
}

impl Exchange {
    async fn run(
        self,
        pending: PendingCall,
        route: ReportRoute,
        outcome: oneshot::Sender<CommandDispatchOutcome>,
    ) {
        let query_bytes = json_size(&self.query, MONITORING_REPORT_OUTPUT_LIMIT_201)
            .unwrap_or(MONITORING_REPORT_OUTPUT_LIMIT_201);
        let seen = Arc::new(AtomicBool::new(false));
        // Poll collection beside the reply so fragments queued behind it never overflow the route.
        let collection = collect(route, self.budget.clone(), query_bytes, seen.clone());
        let reply = pending.receive();
        tokio::pin!(collection, reply);
        let mut early = None;
        let reply = tokio::select! {
            biased;
            reply = &mut reply => reply,
            collected = &mut collection => {
                early = Some(collected);
                reply.await
            }
        };
        let ack = match reply {
            SessionCallOutcome::Result { payload, .. } => match report_ack(&payload) {
                Some(ack) => ack,
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

        let mut retained = None;
        let mut collecting = false;
        let report = if ack == DeviceReportAck201::Accepted {
            // A finished collection future is never polled again, whatever the reply said.
            if let Some(Collected {
                report,
                retained: kept,
            }) = early.take()
            {
                retained = kept;
                report
            } else {
                collecting = true;
                MonitoringReportState201::Pending
            }
        } else if early.is_some() || self.fragments_followed(&mut collection, &seen).await {
            // No report was announced, yet fragments arrived: the charger contradicted itself.
            MonitoringReportState201::Incomplete {
                reason: DeviceReportFailure201::Correlation,
                progress: None,
            }
        } else {
            MonitoringReportState201::NotExpected
        };

        let result = MonitoringResult201 {
            query: self.query.clone(),
            outcome: MonitoringOutcome201::Report {
                ack: Some(ack),
                report,
            },
        };
        drop(
            outcome.send(CommandDispatchOutcome::MonitoringResponse201(Box::new(
                result,
            ))),
        );
        drop(retained);
        if !collecting {
            return;
        }

        let Collected { report, retained } = collection.await;
        self.finish(ack, report).await;
        drop(retained);
    }

    /// Whether the charger already sent fragments behind a reply that announced no report.
    async fn fragments_followed<F>(
        &self,
        collection: &mut std::pin::Pin<&mut F>,
        seen: &AtomicBool,
    ) -> bool
    where
        F: std::future::Future<Output = Collected>,
    {
        // One poll drains whatever the socket owner already queued; nothing is awaited.
        let _ = tokio::time::timeout(Duration::ZERO, collection).await;

        seen.load(Ordering::Acquire)
    }

    /// Completes the report durably with the full evidence, so it does not matter whether the
    /// application has recorded the acknowledgement yet: storage keeps whichever terminal report
    /// arrives first. The command row exists before dispatch, so an unknown request is a lost row
    /// and is not retried. A failed or unknown write is recorded once as such.
    async fn finish(&self, ack: DeviceReportAck201, report: MonitoringReportState201) {
        match self.record(ack, report).await {
            Ok(Some(_)) => {}
            Ok(None) => self.handle.report_store_failed(self.handle.connection_id()),
            Err(_) => {
                self.handle.report_store_failed(self.handle.connection_id());
                let fallback = MonitoringReportState201::Incomplete {
                    reason: DeviceReportFailure201::StorageUnavailable,
                    progress: None,
                };
                let _ = self.record(ack, fallback).await;
            }
        }
    }

    async fn record(
        &self,
        ack: DeviceReportAck201,
        report: MonitoringReportState201,
    ) -> Result<Option<CommandResult>, StorageError> {
        let evidence = MonitoringResult201 {
            query: self.query.clone(),
            outcome: MonitoringOutcome201::Report {
                ack: Some(ack),
                report,
            },
        };
        let lifecycle = monitoring_response_lifecycle(&evidence);

        self.store
            .finish_monitoring_report(self.request.clone(), evidence, lifecycle, self.clock.now())
            .await
    }
}
