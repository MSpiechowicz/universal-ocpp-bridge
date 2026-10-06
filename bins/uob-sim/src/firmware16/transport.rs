//! Native firmware CALL replies, worker-tick progression and ordered status delivery.
use super::{FirmwareHandle, FirmwareMode, FirmwareStatus, StationFacts};
use crate::{Command, Ocpp16State, TraceBuffer, TraceKind};
use ocpp_client::ClientError;
use ocpp_client::ocpp_types::v16::{
    FirmwareStatusNotificationRequest, SignedFirmwareStatusNotificationRequest,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;

/// Only a station configured with a firmware model intercepts its native requests; others keep
/// the client library's `NotImplemented` reply.
pub(crate) fn intercepts(action: &str, state: &Mutex<Ocpp16State>) -> bool {
    matches!(action, "UpdateFirmware" | "SignedUpdateFirmware")
        && state
            .lock()
            .expect("native state lock")
            .firmware16
            .is_some()
}

pub(crate) fn reply(action: &str, payload: &Value, state: &Mutex<Ocpp16State>) -> Value {
    let handle = state.lock().expect("native state lock").firmware16.clone();
    handle.map_or_else(
        || json!({"callError":"NotImplemented"}),
        |handle| handle.handle_call(action, payload, OffsetDateTime::now_utc()),
    )
}

/// Worker tick: advance the model, start downloads and reboots, and claim one status CALL.
pub(crate) fn poll(state: &Arc<Mutex<Ocpp16State>>, traces: &TraceBuffer) {
    let (handle, facts, sender) = {
        let current = state.lock().expect("native state lock");
        let Some(handle) = current.firmware16.clone() else {
            return;
        };
        let registered = current.registered
            && current.socket_connected
            && current.boot_accepted_generation == Some(current.socket_generation);
        let facts = StationFacts {
            idle: current.active_transactions.is_empty(),
            boot_generation: registered.then_some(current.socket_generation),
        };
        let sender = current
            .notifications
            .as_ref()
            .and_then(tokio::sync::mpsc::WeakSender::upgrade);
        (handle, facts, sender)
    };
    let now = OffsetDateTime::now_utc();
    let Ok(effects) = handle.advance(now, facts) else {
        return;
    };
    if let Some(ticket) = effects.download {
        let policy = handle.transfer_policy();
        let model = Arc::downgrade(&handle.0);
        tokio::spawn(async move {
            let image = if ticket.simulated_failure {
                None
            } else {
                crate::artifact_transfer::download(&ticket.location, policy)
                    .await
                    .ok()
                    .map(|artifact| artifact.bytes)
            };
            // A stopped station drops its model; the late result is then discarded.
            if let Some(model) = model.upgrade() {
                let handle = FirmwareHandle(model);
                let _ =
                    handle.download_finished(&ticket, image.as_deref(), OffsetDateTime::now_utc());
            }
        });
    }
    if let Some(version) = effects.reboot {
        let mut current = state.lock().expect("native state lock");
        if current.reset_reason.is_none() {
            if let Some(boot) = current.boot.as_mut().and_then(Value::as_object_mut) {
                boot.insert("firmwareVersion".to_owned(), version.into());
            }
            current.reset_reason = Some(ocpp_client::ocpp_types::v16::common::Reason::Reboot);
        }
        traces.push(TraceKind::ResetReceived, "firmware_reboot");
    }
    let (Some(generation), Some(sender)) = (facts.boot_generation, sender) else {
        return;
    };
    if let Some(status) = handle.next_status(now)
        && sender
            .try_send(Command::FirmwareStatus(status.clone(), generation))
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
    status: FirmwareStatus,
    generation: u64,
) {
    let Some(handle) = state.lock().expect("native state lock").firmware16.clone() else {
        return;
    };
    let delay = handle.status_delay();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let exchange = crate::client_exchange16::NativeExchange::recovery(generation, true);
    let (action, outcome) = match status.kind {
        FirmwareMode::Legacy => {
            let request: Result<FirmwareStatusNotificationRequest, _> =
                serde_json::from_value(json!({"status": status.status}));
            let Ok(request) = request else {
                let _ = handle.status_finished(&status, true, OffsetDateTime::now_utc());
                return;
            };
            traces.push(TraceKind::ChargingCallSent, "FirmwareStatusNotification");
            let outcome = exchange
                .run(client.send_firmware_status_notification(request))
                .await
                .map(|_| ());
            ("FirmwareStatusNotification", outcome)
        }
        FirmwareMode::Signed => {
            let request: Result<SignedFirmwareStatusNotificationRequest, _> =
                serde_json::from_value(
                    json!({"status": status.status, "requestId": status.request_id}),
                );
            let Ok(request) = request else {
                let _ = handle.status_finished(&status, true, OffsetDateTime::now_utc());
                return;
            };
            traces.push(
                TraceKind::ChargingCallSent,
                "SignedFirmwareStatusNotification",
            );
            let outcome = exchange
                .run(client.send_signed_firmware_status_notification(request))
                .await
                .map(|_| ());
            ("SignedFirmwareStatusNotification", outcome)
        }
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
        traces.push(TraceKind::Failed, "firmware_status_uncertain");
    }
}
