use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ocpp_client::ocpp_2_0_1::{OCPP2_0_1Client, OCPP2_0_1Error};
use ocpp_client::ocpp_types::v201::common::{
    CustomData, MessageTriggerEnum as Requested, TriggerMessageStatusEnum as Status,
};
use ocpp_client::ocpp_types::v201::{
    BootNotificationRequest, FirmwareStatusNotificationRequest, HeartbeatRequest,
    LogStatusNotificationRequest, MeterValuesRequest, PublishFirmwareStatusNotificationRequest,
    StatusNotificationRequest, TransactionEventRequest, TriggerMessageRequest,
    TriggerMessageResponse,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::trigger::{TriggerObservation, TriggerReply, TriggerResponses};
use crate::trigger_transport::TriggerBarrier;
use crate::{Ocpp201State, TraceBuffer, TraceKind};

pub(super) struct TriggerJob201 {
    message: Requested,
    payloads: Vec<Value>,
    delay: Duration,
    pub(super) permit: Option<tokio::sync::OwnedSemaphorePermit>,
    pub(super) generation: u64,
}

pub(super) struct TriggerContext201 {
    pub(super) resources: Vec<(u16, u16)>,
    pub(super) responses: TriggerResponses,
    pub(super) observation: TriggerObservation,
    pub(super) state: Arc<Mutex<Ocpp201State>>,
    pub(super) traces: TraceBuffer,
}

pub(super) async fn register(
    client: &OCPP2_0_1Client,
    barrier: TriggerBarrier,
    context: TriggerContext201,
    receiver: mpsc::UnboundedReceiver<TriggerJob201>,
) {
    let TriggerContext201 {
        resources,
        responses,
        observation,
        state,
        traces,
    } = context;
    spawn_dispatcher(client, &barrier, &state, &traces, receiver);

    let callback_barrier = barrier.clone();
    client
        .on_trigger_message(move |request, _| {
            let result = if callback_barrier.discard_invalid() {
                traces.push(TraceKind::TriggerReceived, "invalid:unknown field");
                Err(OCPP2_0_1Error {
                    code: ocpp_client::ocpp_types::v201::RpcErrorCode::PropertyConstraintViolation,
                    description: "TriggerMessage request contains an unknown field".into(),
                    details: json!({}),
                })
            } else {
                let payloads = payloads(&request, &resources, &state);
                let mut status = response_status(&responses, &request.requested_message);
                if matches!(request.requested_message, Requested::BootNotification)
                    && state.lock().expect("OCPP 2.0.1 state lock").registered
                {
                    status = Status::Rejected;
                }
                if status == Status::Accepted && payloads.is_empty() {
                    status = Status::Rejected;
                }
                let action = message_name(&request.requested_message);
                if status == Status::Accepted {
                    let job = TriggerJob201 {
                        message: request.requested_message,
                        payloads: if observation.omit {
                            Vec::new()
                        } else {
                            payloads
                        },
                        delay: Duration::from_millis(observation.delay_ms),
                        permit: None,
                        generation: 0,
                    };
                    if !callback_barrier.accept_201(job) {
                        status = Status::Rejected;
                    }
                } else {
                    callback_barrier.discard();
                }
                traces.push(TraceKind::TriggerReceived, format!("{action}:{status:?}"));
                Ok(TriggerMessageResponse {
                    custom_data: None,
                    status,
                    status_info: None,
                })
            };
            async move { result }
        })
        .await;
    barrier.arm();
}

fn spawn_dispatcher(
    client: &OCPP2_0_1Client,
    barrier: &TriggerBarrier,
    state: &Arc<Mutex<Ocpp201State>>,
    traces: &TraceBuffer,
    mut receiver: mpsc::UnboundedReceiver<TriggerJob201>,
) {
    let dispatcher = client.clone();
    let dispatch_traces = traces.clone();
    let dispatch_state = Arc::clone(state);
    let generation = barrier.generation_handle();
    tokio::spawn(async move {
        while let Some(job) = receiver.recv().await {
            let dispatcher = dispatcher.clone();
            let dispatch_traces = dispatch_traces.clone();
            let dispatch_state = Arc::clone(&dispatch_state);
            let generation = Arc::clone(&generation);
            tokio::spawn(async move {
                if !job.delay.is_zero() {
                    tokio::time::sleep(job.delay).await;
                }
                if job.generation != generation.load(Ordering::SeqCst) {
                    return;
                }
                for payload in job.payloads {
                    if job.generation != generation.load(Ordering::SeqCst) {
                        break;
                    }
                    let action = message_name(&job.message);
                    dispatch_traces.push(TraceKind::ChargingCallSent, action);
                    let transaction =
                        matches!(job.message, Requested::TransactionEvent).then(|| payload.clone());
                    let result = send_call(&dispatcher, &job.message, payload).await;
                    if result.is_ok()
                        && job.generation == generation.load(Ordering::SeqCst)
                        && let Some(transaction) = transaction
                        && let Some(id) = transaction
                            .pointer("/transactionInfo/transactionId")
                            .and_then(Value::as_str)
                        && let Some(current) = dispatch_state
                            .lock()
                            .expect("OCPP 2.0.1 state lock")
                            .transactions
                            .get_mut(id)
                    {
                        current["seqNo"] = transaction["seqNo"].clone();
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
}

fn response_status(responses: &TriggerResponses, message: &Requested) -> Status {
    let reply = match message {
        Requested::BootNotification => responses.boot_notification,
        Requested::LogStatusNotification => responses.log_status_notification,
        Requested::FirmwareStatusNotification => responses.firmware_status_notification,
        Requested::Heartbeat => responses.heartbeat,
        Requested::MeterValues => responses.meter_values,
        Requested::StatusNotification => responses.status_notification,
        Requested::TransactionEvent => responses.transaction_event,
        Requested::PublishFirmwareStatusNotification => {
            responses.publish_firmware_status_notification
        }
        // This simulator has no certificate private key/CSR generator. Never accept a
        // certificate trigger that cannot produce a real signing request.
        Requested::SignChargingStationCertificate
        | Requested::SignV2GCertificate
        | Requested::SignCombinedCertificate => return Status::NotImplemented,
    };
    match reply {
        TriggerReply::Accepted => Status::Accepted,
        TriggerReply::Rejected => Status::Rejected,
        TriggerReply::NotImplemented => Status::NotImplemented,
    }
}

fn message_name(message: &Requested) -> &'static str {
    match message {
        Requested::BootNotification => "BootNotification",
        Requested::LogStatusNotification => "LogStatusNotification",
        Requested::FirmwareStatusNotification => "FirmwareStatusNotification",
        Requested::Heartbeat => "Heartbeat",
        Requested::MeterValues => "MeterValues",
        Requested::StatusNotification => "StatusNotification",
        Requested::TransactionEvent => "TransactionEvent",
        Requested::PublishFirmwareStatusNotification => "PublishFirmwareStatusNotification",
        Requested::SignChargingStationCertificate
        | Requested::SignV2GCertificate
        | Requested::SignCombinedCertificate => "SignCertificate",
    }
}

pub(crate) fn now() -> String {
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

fn scoped_resources(
    request: &TriggerMessageRequest<CustomData>,
    resources: &[(u16, u16)],
    connector_level: bool,
) -> Vec<(u16, u16)> {
    match &request.evse {
        Some(evse) => {
            let Ok(id) = u16::try_from(evse.id) else {
                return Vec::new();
            };
            if connector_level {
                let Some(connector) = evse.connector_id.and_then(|id| u16::try_from(id).ok())
                else {
                    return Vec::new();
                };
                resources
                    .iter()
                    .copied()
                    .filter(|pair| *pair == (id, connector))
                    .collect()
            } else {
                // MeterValues is scoped to EVSE in 2.0.1; the native connector
                // may validate the target, but cannot be represented on that CALL.
                if evse.connector_id.is_some_and(|connector| {
                    !resources
                        .iter()
                        .any(|pair| pair.0 == id && i64::from(pair.1) == connector)
                }) {
                    return Vec::new();
                }
                resources
                    .iter()
                    .copied()
                    .filter(|pair| pair.0 == id)
                    .collect()
            }
        }
        None => resources.to_vec(),
    }
}

fn payloads(
    request: &TriggerMessageRequest<CustomData>,
    resources: &[(u16, u16)],
    state: &Arc<Mutex<Ocpp201State>>,
) -> Vec<Value> {
    let state = state.lock().expect("OCPP 2.0.1 state lock");
    match &request.requested_message {
        Requested::BootNotification => {
            let mut boot = state.boot.clone().unwrap_or_else(|| {
                json!({
                    "chargingStation": {"vendorName": "UOB", "model": "Simulator"}
                })
            });
            boot["reason"] = json!("Triggered");
            vec![boot]
        }
        Requested::Heartbeat => vec![json!({})],
        Requested::LogStatusNotification
        | Requested::FirmwareStatusNotification
        | Requested::PublishFirmwareStatusNotification => vec![json!({"status":"Idle"})],
        Requested::StatusNotification => scoped_resources(request, resources, true)
            .into_iter()
            .map(|(evse, connector)| {
                let mut value = state
                    .status
                    .get(&(evse, connector))
                    .cloned()
                    .unwrap_or_else(|| {
                        json!({
                            "evseId": evse, "connectorId": connector, "connectorStatus": "Available"
                        })
                    });
                value["timestamp"] = json!(now());
                value
            })
            .collect(),
        Requested::MeterValues => meter_payloads(request, resources, &state),
        Requested::TransactionEvent => transaction_payloads(request, resources, &state),
        Requested::SignChargingStationCertificate
        | Requested::SignV2GCertificate
        | Requested::SignCombinedCertificate => Vec::new(),
    }
}

fn meter_payloads(
    request: &TriggerMessageRequest<CustomData>,
    resources: &[(u16, u16)],
    state: &Ocpp201State,
) -> Vec<Value> {
    let mut ids = scoped_resources(request, resources, false)
        .into_iter()
        .map(|(evse, _)| evse)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty()
        || ids.iter().any(|id| {
            state
                .meters
                .get(id)
                .and_then(|reading| reading.get("meterValue"))
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
        })
    {
        return Vec::new();
    }
    ids.into_iter()
        .map(|id| {
            let mut value = state.meters.get(&id).expect("known EVSE readings").clone();
            for meter in value["meterValue"].as_array_mut().expect("meter values") {
                meter["timestamp"] = json!(now());
                for sample in meter["sampledValue"]
                    .as_array_mut()
                    .expect("sampled values")
                {
                    sample["context"] = json!("Trigger");
                }
            }
            value
        })
        .collect()
}

fn transaction_payloads(
    request: &TriggerMessageRequest<CustomData>,
    resources: &[(u16, u16)],
    state: &Ocpp201State,
) -> Vec<Value> {
    let selected = scoped_resources(request, resources, false);
    let mut events = state
        .transactions
        .values()
        .filter(|value| {
            let id = value.pointer("/evse/id").and_then(Value::as_u64);
            let connector = value.pointer("/evse/connectorId").and_then(Value::as_u64);
            selected.iter().any(|(evse, port)| {
                id == Some(u64::from(*evse)) && connector == Some(u64::from(*port))
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    events.sort_by(|a, b| {
        a.pointer("/transactionInfo/transactionId")
            .and_then(Value::as_str)
            .cmp(
                &b.pointer("/transactionInfo/transactionId")
                    .and_then(Value::as_str),
            )
    });
    if events.iter().any(|value| {
        value
            .get("meterValue")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
            || value.pointer("/transactionInfo/chargingState").is_none()
    }) {
        return Vec::new();
    }
    events
        .into_iter()
        .map(|mut value| {
            value["eventType"] = json!("Updated");
            value["triggerReason"] = json!("Trigger");
            value["timestamp"] = json!(now());
            value["seqNo"] = json!(value["seqNo"].as_i64().unwrap_or(0).saturating_add(1));
            value
        })
        .collect()
}

async fn send_call(
    client: &OCPP2_0_1Client,
    message: &Requested,
    payload: Value,
) -> Result<(), String> {
    macro_rules! send {
        ($request:ty, $method:ident) => {{
            let request: $request =
                serde_json::from_value(payload).map_err(|error| error.to_string())?;
            client
                .$method(request)
                .await
                .map_err(|error| error.to_string())?;
            Ok(())
        }};
    }
    match message {
        Requested::BootNotification => {
            send!(BootNotificationRequest<CustomData>, send_boot_notification)
        }
        Requested::LogStatusNotification => send!(
            LogStatusNotificationRequest<CustomData>,
            send_log_status_notification
        ),
        Requested::FirmwareStatusNotification => send!(
            FirmwareStatusNotificationRequest<CustomData>,
            send_firmware_status_notification
        ),
        Requested::Heartbeat => send!(HeartbeatRequest<CustomData>, send_heartbeat),
        Requested::MeterValues => send!(MeterValuesRequest<CustomData>, send_meter_values),
        Requested::StatusNotification => send!(
            StatusNotificationRequest<CustomData>,
            send_status_notification
        ),
        Requested::TransactionEvent => {
            send!(TransactionEventRequest<CustomData>, send_transaction_event)
        }
        Requested::PublishFirmwareStatusNotification => send!(
            PublishFirmwareStatusNotificationRequest<CustomData>,
            send_publish_firmware_status_notification
        ),
        Requested::SignChargingStationCertificate
        | Requested::SignV2GCertificate
        | Requested::SignCombinedCertificate => {
            Err("certificate signing is not configured".to_owned())
        }
    }
}
