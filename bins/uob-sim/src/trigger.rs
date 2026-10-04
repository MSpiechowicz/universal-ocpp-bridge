use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ocpp_client::ocpp_1_6::{OCPP1_6Client, OCPP1_6Error};
use ocpp_client::ocpp_types::v16::RpcErrorCode;
use ocpp_client::ocpp_types::v16::common::{
    TriggerMessageRequestRequestedMessage as Requested, TriggerMessageResponseStatus as Status,
};
use ocpp_client::ocpp_types::v16::{
    BootNotificationRequest, DiagnosticsStatusNotificationRequest,
    FirmwareStatusNotificationRequest, HeartbeatRequest, MeterValuesRequest,
    StatusNotificationRequest, TriggerMessageRequest, TriggerMessageResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::trigger_transport::TriggerBarrier;
use crate::{Ocpp16State, TraceBuffer, TraceKind};

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerReply {
    #[default]
    Accepted,
    Rejected,
    NotImplemented,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TriggerResponses {
    pub boot_notification: TriggerReply,
    pub diagnostics_status_notification: TriggerReply,
    pub firmware_status_notification: TriggerReply,
    pub heartbeat: TriggerReply,
    pub meter_values: TriggerReply,
    pub status_notification: TriggerReply,
    pub log_status_notification: TriggerReply,
    pub publish_firmware_status_notification: TriggerReply,
    pub transaction_event: TriggerReply,
    pub sign_charging_station_certificate: TriggerReply,
    pub sign_v2g_certificate: TriggerReply,
    pub sign_combined_certificate: TriggerReply,
}

impl TriggerResponses {
    fn for_message(&self, message: &Requested) -> Status {
        let reply = match message {
            Requested::BootNotification => self.boot_notification,
            Requested::DiagnosticsStatusNotification => self.diagnostics_status_notification,
            Requested::FirmwareStatusNotification => self.firmware_status_notification,
            Requested::Heartbeat => self.heartbeat,
            Requested::MeterValues => self.meter_values,
            Requested::StatusNotification => self.status_notification,
        };
        match reply {
            TriggerReply::Accepted => Status::Accepted,
            TriggerReply::Rejected => Status::Rejected,
            TriggerReply::NotImplemented => Status::NotImplemented,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TriggerObservation {
    pub delay_ms: u64,
    pub omit: bool,
}

pub(super) struct TriggerJob {
    request: TriggerMessageRequest,
    payloads: Vec<Value>,
    delay: Duration,
    pub(super) permit: Option<tokio::sync::OwnedSemaphorePermit>,
    pub(super) generation: u64,
}

pub(super) struct TriggerSettings {
    pub(super) connectors: Vec<u16>,
    pub(super) responses: TriggerResponses,
    pub(super) observation: TriggerObservation,
}

pub(super) async fn register(
    client: &OCPP1_6Client,
    barrier: TriggerBarrier,
    settings: TriggerSettings,
    state: Arc<Mutex<Ocpp16State>>,
    traces: TraceBuffer,
    mut receiver: mpsc::UnboundedReceiver<TriggerJob>,
) {
    let TriggerSettings {
        connectors,
        responses,
        observation,
    } = settings;

    let dispatcher = client.clone();
    let dispatch_state = Arc::clone(&state);
    let dispatch_traces = traces.clone();
    let generation = barrier.generation_handle();
    tokio::spawn(async move {
        while let Some(job) = receiver.recv().await {
            let dispatcher = dispatcher.clone();
            let dispatch_state = Arc::clone(&dispatch_state);
            let dispatch_traces = dispatch_traces.clone();
            let generation = Arc::clone(&generation);
            tokio::spawn(async move {
                if job.delay != Duration::ZERO {
                    tokio::time::sleep(job.delay).await;
                }
                if job.generation != generation.load(Ordering::SeqCst) {
                    return;
                }
                for payload in job.payloads {
                    if job.generation != generation.load(Ordering::SeqCst) {
                        break;
                    }
                    let action = message_name(&job.request.requested_message);
                    dispatch_traces.push(TraceKind::ChargingCallSent, action);
                    let result =
                        send_call(&dispatcher, &job.request.requested_message, payload).await;
                    if result.as_ref().is_ok_and(|accepted| *accepted) {
                        commit_triggered_boot(&dispatch_state, &generation, job.generation);
                    }
                    dispatch_traces.push(
                        if result.is_ok() {
                            TraceKind::ChargingCallResult
                        } else {
                            TraceKind::Failed
                        },
                        action,
                    );
                }
            });
        }
    });
    let callback_barrier = barrier.clone();
    client
        .on_trigger_message(move |request, _| {
            let result = if callback_barrier.discard_invalid() {
                traces.push(TraceKind::TriggerReceived, "invalid:unknown field");
                Err(OCPP1_6Error {
                    code: RpcErrorCode::PropertyConstraintViolation,
                    description: "TriggerMessage request contains an unknown field".into(),
                    details: json!({}),
                })
            } else {
                let payloads = payloads(&request, &connectors, &state);
                let mut status = responses.for_message(&request.requested_message);
                if matches!(&request.requested_message, Requested::BootNotification) && {
                    let state = state.lock().expect("OCPP 1.6 state lock");
                    state.boot_accepted_generation == Some(state.socket_generation)
                } {
                    status = Status::Rejected;
                }
                if status == Status::Accepted && payloads.is_empty() {
                    status = Status::Rejected;
                }
                let action = message_name(&request.requested_message);
                if status == Status::Accepted {
                    let job = TriggerJob {
                        request,
                        payloads: if observation.omit {
                            Vec::new()
                        } else {
                            payloads
                        },
                        delay: Duration::from_millis(observation.delay_ms),
                        permit: None,
                        generation: 0,
                    };
                    if !callback_barrier.accept(job) {
                        status = Status::Rejected;
                    }
                } else {
                    callback_barrier.discard();
                }
                traces.push(TraceKind::TriggerReceived, format!("{action}:{status:?}"));
                Ok(TriggerMessageResponse { status })
            };
            async move { result }
        })
        .await;
    barrier.arm();
}

fn can_register_triggered_boot(state: &Ocpp16State) -> bool {
    state.socket_connected && state.boot_accepted_generation == Some(state.socket_generation)
}

fn commit_triggered_boot(state: &Mutex<Ocpp16State>, generation: &AtomicU64, job_generation: u64) {
    let mut state = state.lock().expect("OCPP 1.6 state lock");
    if job_generation == generation.load(Ordering::SeqCst) && can_register_triggered_boot(&state) {
        state.registered = true;
        state.replay_requested = true;
    }
}

fn message_name(message: &Requested) -> &'static str {
    match message {
        Requested::BootNotification => "BootNotification",
        Requested::DiagnosticsStatusNotification => "DiagnosticsStatusNotification",
        Requested::FirmwareStatusNotification => "FirmwareStatusNotification",
        Requested::Heartbeat => "Heartbeat",
        Requested::MeterValues => "MeterValues",
        Requested::StatusNotification => "StatusNotification",
    }
}

fn scoped_connectors(
    request: &TriggerMessageRequest,
    connectors: &[u16],
    station: bool,
) -> Vec<u16> {
    match request.connector_id {
        Some(0) if station => vec![0],
        Some(0) => Vec::new(),
        Some(id) => u16::try_from(id)
            .ok()
            .filter(|id| connectors.contains(id))
            .into_iter()
            .collect(),
        None if station => std::iter::once(0)
            .chain(connectors.iter().copied())
            .collect(),
        None => connectors.to_vec(),
    }
}

fn now() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    ocpp_client::ocpp_types::OcppTimestamp::from_unix(
        i64::try_from(elapsed.as_secs()).expect("unix timestamp exceeds i64::MAX seconds"),
        0,
    )
    .expect("valid unix timestamp")
    .to_string()
}

fn payloads(
    request: &TriggerMessageRequest,
    connectors: &[u16],
    state: &Arc<Mutex<Ocpp16State>>,
) -> Vec<Value> {
    let state = state.lock().expect("OCPP 1.6 state lock");
    match &request.requested_message {
        Requested::BootNotification => vec![state.boot.clone().unwrap_or_else(|| {
            json!({
                "chargePointVendor": "UOB", "chargePointModel": "Simulator"
            })
        })],
        Requested::DiagnosticsStatusNotification | Requested::FirmwareStatusNotification => {
            vec![json!({"status":"Idle"})]
        }
        Requested::Heartbeat => vec![json!({})],
        Requested::StatusNotification => {
            let ids = scoped_connectors(request, connectors, true);
            ids.into_iter()
                .map(|id| {
                    state.status.get(&id).cloned().unwrap_or_else(|| {
                        json!({
                            "connectorId":id, "status":"Available", "errorCode":"NoError"
                        })
                    })
                })
                .collect()
        }
        Requested::MeterValues => {
            let ids = scoped_connectors(request, connectors, false);
            if ids.is_empty() || ids.iter().any(|id| !state.meters.contains_key(id)) {
                return Vec::new();
            }
            let timestamp = now();
            ids.into_iter()
                .map(|id| {
                    let mut value = state.meters.get(&id).expect("known meter reading").clone();
                    value
                        .as_object_mut()
                        .expect("meter payload object")
                        .remove("transactionId");
                    for item in value["meterValue"]
                        .as_array_mut()
                        .expect("meter value array")
                    {
                        item["timestamp"] = json!(timestamp);
                        for sample in item["sampledValue"]
                            .as_array_mut()
                            .expect("sampled value array")
                        {
                            sample["context"] = json!("Trigger");
                        }
                    }
                    value
                })
                .collect()
        }
    }
}

async fn send_call(
    client: &OCPP1_6Client,
    message: &Requested,
    payload: Value,
) -> Result<bool, String> {
    macro_rules! send {
        ($request:ty, $method:ident) => {{
            let request: $request =
                serde_json::from_value(payload).map_err(|error| error.to_string())?;
            client
                .$method(request)
                .await
                .map_err(|error| error.to_string())?;
            Ok(false)
        }};
    }
    match message {
        Requested::BootNotification => {
            let request: BootNotificationRequest =
                serde_json::from_value(payload).map_err(|error| error.to_string())?;
            let response = client
                .send_boot_notification(request)
                .await
                .map_err(|error| error.to_string())?;
            Ok(serde_json::to_value(response)
                .ok()
                .and_then(|value| value.get("status").cloned())
                == Some(json!("Accepted")))
        }
        Requested::DiagnosticsStatusNotification => send!(
            DiagnosticsStatusNotificationRequest,
            send_diagnostics_status_notification
        ),
        Requested::FirmwareStatusNotification => send!(
            FirmwareStatusNotificationRequest,
            send_firmware_status_notification
        ),
        Requested::Heartbeat => send!(HeartbeatRequest, send_heartbeat),
        Requested::MeterValues => send!(MeterValuesRequest, send_meter_values),
        Requested::StatusNotification => send!(StatusNotificationRequest, send_status_notification),
    }
}
