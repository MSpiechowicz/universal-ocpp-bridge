use std::sync::{Arc, Mutex};

use ocpp_client::ocpp_types::v201::common::{
    CustomData, RequestStartStopStatusEnum, ResetStatusEnum,
};
use ocpp_client::ocpp_types::v201::{
    AuthorizeRequest, BootNotificationRequest, HeartbeatRequest, RequestStartTransactionResponse,
    RequestStopTransactionResponse, ResetResponse, StatusNotificationRequest,
    TransactionEventRequest,
};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::{
    Command, Ocpp201State, OcppVersion, RemoteCommand, RemoteCommandKind, ReplyDelaySlot,
    SimulatorAction, SimulatorCall, SimulatorClientError, TraceBuffer, TraceKind, take_reply_delay,
};

pub(super) async fn register_handlers(
    client: &ocpp_client::ocpp_2_0_1::OCPP2_0_1Client,
    traces: &TraceBuffer,
    remote_commands: mpsc::Sender<RemoteCommand>,
    resources: Vec<(u16, u16)>,
    state: Arc<Mutex<Ocpp201State>>,
    reply_delay: ReplyDelaySlot,
) {
    let reset_traces = traces.clone();
    client
        .on_reset(move |_request, _client| {
            reset_traces.push(TraceKind::ResetReceived, "accepted");
            async move {
                Ok(ResetResponse {
                    custom_data: None,
                    status: ResetStatusEnum::Accepted,
                    status_info: None,
                })
            }
        })
        .await;

    let start_traces = traces.clone();
    let commands = remote_commands.clone();
    let start_state = Arc::clone(&state);
    let start_delay = Arc::clone(&reply_delay);
    client
        .on_request_start_transaction(move |request, _client| {
            let evse_id = request.evse_id.and_then(|value| u16::try_from(value).ok());
            let accepted = evse_id.is_none_or(|evse| {
                resources.iter().any(|(configured, _)| *configured == evse)
                    && !start_state
                        .lock()
                        .expect("OCPP 2.0.1 state lock poisoned")
                        .active_transactions
                        .values()
                        .any(|(active, _)| *active == evse)
            });
            start_traces.push(
                TraceKind::RemoteStartReceived,
                if accepted { "accepted" } else { "rejected" },
            );
            let payload = serde_json::to_value(&request).unwrap_or(serde_json::Value::Null);
            let delayed_reply = take_reply_delay(&start_delay, RemoteCommandKind::StartTransaction);
            let _ = commands.try_send(RemoteCommand {
                kind: RemoteCommandKind::StartTransaction,
                payload,
                accepted,
            });
            async move {
                if let Some(delay) = delayed_reply {
                    tokio::time::sleep(delay.duration).await;
                    let _ = delay.receipt.send(());
                }
                Ok(RequestStartTransactionResponse {
                    custom_data: None,
                    status: status(accepted),
                    status_info: None,
                    transaction_id: None,
                })
            }
        })
        .await;

    let stop_traces = traces.clone();
    let stop_delay = reply_delay;
    client
        .on_request_stop_transaction(move |request, _client| {
            let accepted = state
                .lock()
                .expect("OCPP 2.0.1 state lock poisoned")
                .active_transactions
                .contains_key(request.transaction_id.as_str());
            stop_traces.push(
                TraceKind::RemoteStopReceived,
                if accepted { "accepted" } else { "rejected" },
            );
            let payload = serde_json::to_value(&request).unwrap_or(serde_json::Value::Null);
            let delayed_reply = take_reply_delay(&stop_delay, RemoteCommandKind::StopTransaction);
            let _ = remote_commands.try_send(RemoteCommand {
                kind: RemoteCommandKind::StopTransaction,
                payload,
                accepted,
            });
            async move {
                if let Some(delay) = delayed_reply {
                    tokio::time::sleep(delay.duration).await;
                    let _ = delay.receipt.send(());
                }
                Ok(RequestStopTransactionResponse {
                    custom_data: None,
                    status: status(accepted),
                    status_info: None,
                })
            }
        })
        .await;
}

const fn status(accepted: bool) -> RequestStartStopStatusEnum {
    if accepted {
        RequestStartStopStatusEnum::Accepted
    } else {
        RequestStartStopStatusEnum::Rejected
    }
}

pub(super) async fn run(
    client: ocpp_client::ocpp_2_0_1::OCPP2_0_1Client,
    mut commands: mpsc::Receiver<Command>,
    traces: TraceBuffer,
    outstanding_capacity: usize,
    mut remote_commands: mpsc::Receiver<RemoteCommand>,
    state: Arc<Mutex<Ocpp201State>>,
) {
    let mut requests = JoinSet::new();
    let mut replay = JoinSet::new();
    let mut reservations = JoinSet::new();
    let mut replay_generation = None;
    let mut housekeeping = tokio::time::interval(std::time::Duration::from_millis(50));
    loop {
        tokio::select! {
            _ = housekeeping.tick(), if replay.is_empty() => {
                crate::reservation201::transport::tick(&client, &state, &mut reservations, &traces);
                let candidate = {
                    let current = state.lock().expect("native state lock");
                    (current.registered && current.socket_connected && replay_generation != Some(current.socket_generation))
                        .then(|| (current.socket_generation, current.local.clone()))
                };
                if let Some((generation, local)) = candidate {
                    replay_generation = Some(generation);
                    if let Some(local) = local.filter(crate::local_authorization201::LocalAuthorization201Handle::has_persistence) {
                        let client = client.clone();
                        let state = Arc::clone(&state);
                        let traces = traces.clone();
                        replay.spawn(async move {
                            if local.replay_native(&client, &state, generation).await.is_err() {
                                traces.push(TraceKind::Failed, "native offline replay unavailable or uncertain");
                            }
                        });
                    }
                }
            }
            _ = replay.join_next(), if !replay.is_empty() => {}
            _ = reservations.join_next(), if !reservations.is_empty() => {}
            _ = requests.join_next(), if !requests.is_empty() => {}
            command = commands.recv(), if requests.len() < outstanding_capacity => match command {
                Some(Command::Heartbeat(result)) => {
                    let client = client.clone();
                    let traces = traces.clone();
                    requests.spawn(async move {
                        traces.push(TraceKind::HeartbeatSent, "Heartbeat");
                        let response = client.send_heartbeat(HeartbeatRequest { custom_data: None }).await
                            .map(|response| response.current_time.to_string())
                            .map_err(|_| SimulatorClientError::Protocol("native heartbeat failed".to_owned()));
                        record_result(&traces, &response);
                        let _ = result.send(response);
                    });
                }
                Some(Command::Call(mut call, result)) => {
                    let client = client.clone();
                    let traces = traces.clone();
                    let state = Arc::clone(&state);
                    requests.spawn(async move {
                        let wire_name = call.action.wire_name(OcppVersion::V2_0_1);
                        traces.push(TraceKind::ChargingCallSent, wire_name);
                        let _hold = crate::reservation201::transport::OutboundHold::new(&state);
                        let generation = state.lock().expect("native state lock").socket_generation;
                        // Actual reservation facts bind before the exact original event is captured.
                        if let Err(error) = Box::pin(crate::reservation201::transport::prepare(&client, &state, &mut call, generation)).await {
                            traces.push(TraceKind::Failed, wire_name);
                            let _ = result.send(Err(error));
                            return;
                        }
                        let response = crate::local_authorization201::transport::on_generation(
                            generation,
                            (call.payload.get("eventType").is_some()).then(|| call.payload.clone()),
                            Box::pin(send_call(&client, &call)),
                        ).await;
                        let current_generation = state.lock().expect("native state lock").socket_generation == generation;
                        if let Ok(value) = &response && current_generation {
                            update_state(&state, &call, value);
                        }
                        traces.push(if response.is_ok() { TraceKind::ChargingCallResult } else { TraceKind::Failed }, wire_name);
                        let _ = result.send(response);
                    });
                }
                Some(Command::NextRemote(result)) => {
                    let response = remote_commands.recv().await.ok_or(SimulatorClientError::Stopped);
                    let _ = result.send(response);
                }
                Some(Command::LocalListConflict) => unreachable!("native notification sender exists only for OCPP 1.6"),
                Some(Command::ReservationStatus(_, _) | Command::FirmwareStatus(_, _)) => unreachable!("native notifications are OCPP 1.6 only"),
                Some(Command::Shutdown(result)) => {
                    requests.shutdown().await;
                    replay.shutdown().await;
                    reservations.shutdown().await;
                    let response = client.disconnect().await
                        .map_err(|_| SimulatorClientError::Protocol("native disconnect failed".to_owned()));
                    state.lock().expect("native state lock").local = None;
                    state.lock().expect("native state lock").reservation201 = None;
                    traces.push(TraceKind::Stopped, "client disconnected");
                    let _ = result.send(response);
                    break;
                }
                None => { requests.shutdown().await; replay.shutdown().await; reservations.shutdown().await; break; }
            }
        }
    }
    state.lock().expect("native state lock").reservation201 = None;
}

async fn send_call(
    client: &ocpp_client::ocpp_2_0_1::OCPP2_0_1Client,
    call: &SimulatorCall,
) -> Result<serde_json::Value, SimulatorClientError> {
    macro_rules! exchange {
        ($request:ty, $method:ident) => {{
            let request: $request = serde_json::from_value(call.payload.clone())
                .map_err(|_| SimulatorClientError::Protocol("invalid native request".to_owned()))?;
            let response = client
                .$method(request)
                .await
                .map_err(|_| SimulatorClientError::Protocol("native exchange failed".to_owned()))?;
            serde_json::to_value(response)
                .map_err(|_| SimulatorClientError::Protocol("invalid native response".to_owned()))
        }};
    }
    match call.action {
        SimulatorAction::BootNotification => {
            exchange!(BootNotificationRequest<CustomData>, send_boot_notification)
        }
        SimulatorAction::Authorize => exchange!(AuthorizeRequest<CustomData>, send_authorize),
        SimulatorAction::StatusNotification => {
            exchange!(
                StatusNotificationRequest<CustomData>,
                send_status_notification
            )
        }
        SimulatorAction::StartTransaction
        | SimulatorAction::MeterValues
        | SimulatorAction::StopTransaction => {
            exchange!(TransactionEventRequest<CustomData>, send_transaction_event)
        }
    }
}

fn update_state(
    state: &Arc<Mutex<Ocpp201State>>,
    call: &SimulatorCall,
    response: &serde_json::Value,
) {
    let mut state = state.lock().expect("OCPP 2.0.1 state lock poisoned");
    match call.action {
        SimulatorAction::BootNotification => {
            state.boot = Some(call.payload.clone());
            // Registration is set only by a correlated current-socket Boot reply
            // in the owned native transport, never by a late worker result.
        }
        SimulatorAction::StatusNotification => {
            if let (Some(evse), Some(connector)) = (
                unsigned_id(&call.payload, "/evseId"),
                unsigned_id(&call.payload, "/connectorId"),
            ) {
                state.status.insert((evse, connector), call.payload.clone());
            }
        }
        SimulatorAction::MeterValues
        | SimulatorAction::StartTransaction
        | SimulatorAction::StopTransaction => {
            update_transaction(&mut state, call, response);
        }
        SimulatorAction::Authorize => {}
    }
}

fn update_transaction(
    state: &mut Ocpp201State,
    call: &SimulatorCall,
    response: &serde_json::Value,
) {
    let Some(transaction_id) = call
        .payload
        .pointer("/transactionInfo/transactionId")
        .and_then(serde_json::Value::as_str)
    else {
        return;
    };
    match call
        .payload
        .get("eventType")
        .and_then(serde_json::Value::as_str)
    {
        Some("Started")
            if response
                .pointer("/idTokenInfo/status")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|status| status == "Accepted") =>
        {
            if let (Some(evse), Some(connector)) = (
                unsigned_id(&call.payload, "/evse/id"),
                unsigned_id(&call.payload, "/evse/connectorId"),
            ) {
                state
                    .active_transactions
                    .insert(transaction_id.to_owned(), (evse, connector));
                state
                    .transactions
                    .insert(transaction_id.to_owned(), call.payload.clone());
            }
        }
        Some("Updated") if state.active_transactions.contains_key(transaction_id) => {
            let previous = state
                .transactions
                .entry(transaction_id.to_owned())
                .or_insert_with(|| call.payload.clone());
            let mut current = call.payload.clone();
            if current.get("meterValue").is_none() {
                current["meterValue"] = previous
                    .get("meterValue")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
            }
            if current.get("evse").is_none() {
                current["evse"] = previous
                    .get("evse")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
            }
            if current.pointer("/transactionInfo/chargingState").is_none()
                && let Some(charging_state) = previous.pointer("/transactionInfo/chargingState")
            {
                current["transactionInfo"]["chargingState"] = charging_state.clone();
            }
            *previous = current;
        }
        Some("Ended") => {
            state.active_transactions.remove(transaction_id);
            state.transactions.remove(transaction_id);
        }
        _ => {}
    }
    if state.active_transactions.contains_key(transaction_id)
        && let Some(evse) = unsigned_id(&call.payload, "/evse/id")
        && let Some(values) = call.payload.get("meterValue")
    {
        state.meters.insert(
            evse,
            serde_json::json!({
                "evseId": evse, "meterValue": values
            }),
        );
    }
}

fn unsigned_id(payload: &serde_json::Value, pointer: &str) -> Option<u16> {
    payload
        .pointer(pointer)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
}

fn record_result(traces: &TraceBuffer, response: &Result<String, SimulatorClientError>) {
    match response {
        Ok(timestamp) => traces.push(TraceKind::HeartbeatResult, timestamp),
        Err(_) => traces.push(TraceKind::Failed, "native heartbeat failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenless_started_response_keeps_native_transaction_active_until_end() {
        let mut state = Ocpp201State::default();
        let mut call = SimulatorCall {
            action: SimulatorAction::StartTransaction,
            payload: serde_json::json!({
                "eventType":"Started",
                "transactionInfo":{"transactionId":"native-tx"},
                "evse":{"id":1,"connectorId":1}
            }),
        };
        update_transaction(&mut state, &call, &serde_json::json!({}));
        assert_eq!(state.active_transactions.get("native-tx"), Some(&(1, 1)));
        call.payload["eventType"] = serde_json::json!("Ended");
        update_transaction(&mut state, &call, &serde_json::json!({}));
        assert!(!state.active_transactions.contains_key("native-tx"));

        call.payload["eventType"] = serde_json::json!("Started");
        update_transaction(
            &mut state,
            &call,
            &serde_json::json!({"idTokenInfo":{"status":"Invalid"}}),
        );
        assert!(!state.active_transactions.contains_key("native-tx"));
    }
}
