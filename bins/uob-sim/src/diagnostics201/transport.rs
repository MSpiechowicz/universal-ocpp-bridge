//! Native `GetLog` replies, worker-tick progression, uploads and ordered `LogStatusNotification`
//! delivery on the socket generation that accepted the Boot.
use super::{Diagnostics201Handle, LogStatus201};
use crate::{Ocpp201State, TraceBuffer, TraceKind};
use ocpp_client::ClientError;
use ocpp_client::ocpp_2_0_1::OCPP2_0_1Client;
use ocpp_client::ocpp_types::v201::LogStatusNotificationRequest;
use ocpp_client::ocpp_types::v201::common::CustomData;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;
use tokio::task::JoinSet;

fn handle(state: &Mutex<Ocpp201State>) -> Option<Diagnostics201Handle> {
    state
        .lock()
        .expect("native state lock")
        .diagnostics201
        .clone()
}

/// Only a station configured with a log model intercepts `GetLog`; others keep the client
/// library's `NotImplemented` reply.
pub(crate) fn intercepts(action: &str, state: &Mutex<Ocpp201State>) -> bool {
    action == "GetLog" && handle(state).is_some()
}

pub(crate) fn reply(payload: &Value, state: &Mutex<Ocpp201State>) -> Value {
    handle(state).map_or_else(
        || json!({"callError":"NotImplemented"}),
        |handle| handle.handle_call(payload),
    )
}

/// Triggered status from the model (N01.FR.13); `Idle` for a station without one.
pub(crate) fn trigger_payload(state: &Mutex<Ocpp201State>) -> Value {
    // Never hold the station state mutex while inspecting the log model mutex.
    handle(state).map_or_else(
        || json!({"status":"Idle"}),
        |handle| handle.trigger_payload(),
    )
}

/// Worker tick: advance the model, start uploads and send one status CALL.
pub(crate) fn tick(
    client: &OCPP2_0_1Client,
    state: &Arc<Mutex<Ocpp201State>>,
    tasks: &mut JoinSet<()>,
    traces: &TraceBuffer,
) {
    let (handle, generation, registered) = {
        let current = state.lock().expect("native state lock");
        let Some(handle) = current.diagnostics201.clone() else {
            return;
        };
        (
            handle,
            current.socket_generation,
            current.registered && current.socket_connected,
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
                let _ = Diagnostics201Handle(model).upload_finished(
                    &ticket,
                    success,
                    OffsetDateTime::now_utc(),
                );
            }
        });
    }
    if !registered || !tasks.is_empty() {
        return;
    }
    if let Some(status) = handle.next_status(now) {
        tasks.spawn(send(
            client.clone(),
            handle,
            traces.clone(),
            status,
            generation,
        ));
    }
}

async fn send(
    client: OCPP2_0_1Client,
    handle: Diagnostics201Handle,
    traces: TraceBuffer,
    status: LogStatus201,
    generation: u64,
) {
    let delay = handle.status_delay();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    // N01.FR.07: every status carries the requestId of the GetLog that started the upload.
    let request: Result<LogStatusNotificationRequest<CustomData>, _> =
        serde_json::from_value(json!({"status": status.status, "requestId": status.request_id}));
    let Ok(request) = request else {
        let _ = handle.status_finished(&status, true, OffsetDateTime::now_utc());
        return;
    };
    traces.push(TraceKind::ChargingCallSent, "LogStatusNotification");
    let outcome = crate::local_authorization201::transport::on_generation(
        generation,
        None,
        client.send_log_status_notification(request),
    )
    .await
    .map(|_| ());
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
        "LogStatusNotification",
    );
    if handle
        .status_finished(&status, delivered, OffsetDateTime::now_utc())
        .is_err()
    {
        traces.push(TraceKind::Failed, "diagnostics_status_uncertain");
    }
}
