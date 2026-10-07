//! Native log-request CALL replies, worker-tick progression and ordered status delivery.
use super::{DiagnosticsHandle, LogStatus};
use crate::{Command, Ocpp16State, TraceBuffer, TraceKind};
use ocpp_client::ClientError;
use ocpp_client::ocpp_types::v16::{
    DiagnosticsStatusNotificationRequest, LogStatusNotificationRequest,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;

/// Only a station configured with a diagnostics model intercepts its native requests; others
/// keep the client library's `NotImplemented` reply.
pub(crate) fn intercepts(action: &str, state: &Mutex<Ocpp16State>) -> bool {
    matches!(action, "GetDiagnostics" | "GetLog")
        && state
            .lock()
            .expect("native state lock")
            .diagnostics16
            .is_some()
}

pub(crate) fn reply(action: &str, payload: &Value, state: &Mutex<Ocpp16State>) -> Value {
    let handle = state
        .lock()
        .expect("native state lock")
        .diagnostics16
        .clone();
    handle.map_or_else(
        || json!({"callError":"NotImplemented"}),
        |handle| handle.handle_call(action, payload),
    )
}

/// Worker tick: advance the model, start uploads and claim one status CALL.
pub(crate) fn poll(state: &Arc<Mutex<Ocpp16State>>) {
    let (handle, generation, sender) = {
        let current = state.lock().expect("native state lock");
        let Some(handle) = current.diagnostics16.clone() else {
            return;
        };
        let registered = current.registered
            && current.socket_connected
            && current.boot_accepted_generation == Some(current.socket_generation);
        let sender = current
            .notifications
            .as_ref()
            .and_then(tokio::sync::mpsc::WeakSender::upgrade);
        (
            handle,
            registered.then_some(current.socket_generation),
            sender,
        )
    };
    let now = OffsetDateTime::now_utc();
    let Ok(ticket) = handle.advance(now) else {
        return;
    };
    if let Some(ticket) = ticket {
        let policy = handle.transfer_policy();
        let model = Arc::downgrade(&handle.0);
        tokio::spawn(async move {
            let success = !ticket.simulated_failure
                && crate::artifact_transfer::upload(
                    &ticket.location,
                    &ticket.file_name,
                    ticket.content.clone(),
                    policy,
                )
                .await
                .is_ok();
            // A stopped station drops its model; the late result is then discarded.
            if let Some(model) = model.upgrade() {
                let _ = DiagnosticsHandle(model).upload_finished(
                    &ticket,
                    success,
                    OffsetDateTime::now_utc(),
                );
            }
        });
    }
    let (Some(generation), Some(sender)) = (generation, sender) else {
        return;
    };
    if let Some(status) = handle.next_status(now)
        && sender
            .try_send(Command::DiagnosticsStatus(status.clone(), generation))
            .is_err()
    {
        let _ = handle.status_finished(&status, false, now);
    }
}

/// Deliver one claimed status on the socket generation that accepted the current Boot.
pub(crate) async fn send(
    client: ocpp_client::ocpp_1_6::OCPP1_6Client,
    state: Arc<Mutex<Ocpp16State>>,
    traces: TraceBuffer,
    status: LogStatus,
    generation: u64,
) {
    let Some(handle) = state
        .lock()
        .expect("native state lock")
        .diagnostics16
        .clone()
    else {
        return;
    };
    let delay = handle.status_delay();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let exchange = crate::client_exchange16::NativeExchange::recovery(generation, true);
    let (action, outcome) = if status.kind.legacy() {
        let request: Result<DiagnosticsStatusNotificationRequest, _> =
            serde_json::from_value(json!({"status": status.status}));
        let Ok(request) = request else {
            let _ = handle.status_finished(&status, true, OffsetDateTime::now_utc());
            return;
        };
        traces.push(TraceKind::ChargingCallSent, "DiagnosticsStatusNotification");
        let outcome = exchange
            .run(client.send_diagnostics_status_notification(request))
            .await
            .map(|_| ());
        ("DiagnosticsStatusNotification", outcome)
    } else {
        // N01.FR.07: every status carries the requestId of the GetLog that started it.
        let request: Result<LogStatusNotificationRequest, _> = serde_json::from_value(
            json!({"status": status.status, "requestId": status.request_id}),
        );
        let Ok(request) = request else {
            let _ = handle.status_finished(&status, true, OffsetDateTime::now_utc());
            return;
        };
        traces.push(TraceKind::ChargingCallSent, "LogStatusNotification");
        let outcome = exchange
            .run(client.send_log_status_notification(request))
            .await
            .map(|_| ());
        ("LogStatusNotification", outcome)
    };
    // A CALLERROR or undecodable CALLRESULT was still received by the CSMS: never resend it.
    let delivered = matches!(
        outcome,
        Ok(()) | Err(ClientError::Protocol(_) | ClientError::Decode(_))
    );
    traces.push(
        if outcome.is_ok() {
            TraceKind::ChargingCallResult
        } else {
            TraceKind::Failed
        },
        action,
    );
    if handle
        .status_finished(&status, delivered, OffsetDateTime::now_utc())
        .is_err()
    {
        traces.push(TraceKind::Failed, "diagnostics_status_uncertain");
    }
}
