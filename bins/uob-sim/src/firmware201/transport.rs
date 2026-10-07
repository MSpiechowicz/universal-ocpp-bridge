//! Native `UpdateFirmware` replies, worker-tick progression, firmware reboots and ordered
//! `FirmwareStatusNotification` delivery on the socket generation that accepted the Boot.
use super::{Firmware201Handle, FirmwareStatus201, StationFacts201};
use crate::{Ocpp201State, TraceBuffer, TraceKind};
use ocpp_client::ClientError;
use ocpp_client::ocpp_2_0_1::OCPP2_0_1Client;
use ocpp_client::ocpp_types::v201::FirmwareStatusNotificationRequest;
use ocpp_client::ocpp_types::v201::common::CustomData;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;
use tokio::task::JoinSet;

fn handle(state: &Mutex<Ocpp201State>) -> Option<Firmware201Handle> {
    state.lock().expect("native state lock").firmware201.clone()
}

/// Only a station configured with a firmware model intercepts `UpdateFirmware`; others keep
/// the client library's `NotImplemented` reply.
pub(crate) fn intercepts(action: &str, state: &Mutex<Ocpp201State>) -> bool {
    action == "UpdateFirmware" && handle(state).is_some()
}

pub(crate) fn reply(payload: &Value, state: &Mutex<Ocpp201State>) -> Value {
    handle(state).map_or_else(
        || json!({"callError":"NotImplemented"}),
        |handle| handle.handle_call(payload, OffsetDateTime::now_utc()),
    )
}

/// Triggered status from the model; `Idle` for a station without one.
pub(crate) fn trigger_payload(state: &Mutex<Ocpp201State>) -> Value {
    // Never hold the station state mutex while inspecting the firmware model mutex.
    handle(state).map_or_else(
        || json!({"status":"Idle"}),
        |handle| handle.trigger_payload(),
    )
}

/// Worker tick: advance the model, start downloads and reboots, and send one status CALL.
pub(crate) fn tick(
    client: &OCPP2_0_1Client,
    state: &Arc<Mutex<Ocpp201State>>,
    tasks: &mut JoinSet<()>,
    traces: &TraceBuffer,
) {
    let (handle, facts, generation) = {
        let current = state.lock().expect("native state lock");
        let Some(handle) = current.firmware201.clone() else {
            return;
        };
        let registered = current.registered && current.socket_connected;
        let facts = StationFacts201 {
            idle: current.active_transactions.is_empty(),
            boot_generation: registered.then_some(current.socket_generation),
        };
        (handle, facts, current.socket_generation)
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
                let handle = Firmware201Handle(model);
                let _ =
                    handle.download_finished(&ticket, image.as_deref(), OffsetDateTime::now_utc());
            }
        });
    }
    if let Some(version) = effects.reboot {
        reboot(state, &version);
        traces.push(TraceKind::ResetReceived, "firmware_reboot");
        return;
    }
    if facts.boot_generation.is_none() || !tasks.is_empty() {
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

/// Activate the new firmware: the next Boot reports its version and reason, and the current
/// socket closes so the station reconnects as the rebooted firmware.
fn reboot(state: &Mutex<Ocpp201State>, version: &str) {
    let close = {
        let mut current = state.lock().expect("native state lock");
        if let Some(boot) = current.boot.as_mut() {
            boot["reason"] = "FirmwareUpdate".into();
            if let Some(station) = boot
                .get_mut("chargingStation")
                .and_then(Value::as_object_mut)
            {
                station.insert("firmwareVersion".to_owned(), version.into());
            }
        }
        current.reboot_count += 1;
        current.socket_close.clone()
    };
    if let Some(close) = close {
        close.notify_one();
    }
}

async fn send(
    client: OCPP2_0_1Client,
    handle: Firmware201Handle,
    traces: TraceBuffer,
    status: FirmwareStatus201,
    generation: u64,
) {
    let delay = handle.status_delay();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let request: Result<FirmwareStatusNotificationRequest<CustomData>, _> =
        serde_json::from_value(json!({"status": status.status, "requestId": status.request_id}));
    let Ok(request) = request else {
        let _ = handle.status_finished(&status, true, OffsetDateTime::now_utc());
        return;
    };
    traces.push(TraceKind::ChargingCallSent, "FirmwareStatusNotification");
    let outcome = crate::local_authorization201::transport::on_generation(
        generation,
        None,
        client.send_firmware_status_notification(request),
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
        "FirmwareStatusNotification",
    );
    if handle
        .status_finished(&status, delivered, OffsetDateTime::now_utc())
        .is_err()
    {
        traces.push(TraceKind::Failed, "firmware_status_uncertain");
    }
}
