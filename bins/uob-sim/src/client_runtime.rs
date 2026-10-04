use std::sync::{Arc, Mutex};

use crate::client_observation16::observe_local_authorization;
use ocpp_client::ocpp_types::v16::common::{
    RemoteStartTransactionResponseStatus, RemoteStopTransactionResponseStatus,
};
use ocpp_client::ocpp_types::v16::{
    BootNotificationRequest, HeartbeatRequest as HeartbeatRequest16, MeterValuesRequest,
    RemoteStartTransactionResponse, RemoteStopTransactionResponse, StatusNotificationRequest,
};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::{
    Command, EmergencyClient, Ocpp16State, OcppVersion, RemoteCommand, RemoteCommandKind,
    ReplyDelaySlot, SimulatorAction, SimulatorCall, SimulatorClientConfig, SimulatorClientError,
    TraceBuffer, TraceKind, take_reply_delay,
};

#[allow(clippy::too_many_arguments)]
pub(super) async fn connect_and_run_1_6(
    config: &SimulatorClientConfig,
    traces: &TraceBuffer,
    remote_commands: mpsc::Sender<RemoteCommand>,
    commands: mpsc::Receiver<Command>,
    remote_receiver: mpsc::Receiver<RemoteCommand>,
    state: Arc<Mutex<Ocpp16State>>,
    reply_delay: ReplyDelaySlot,
) -> Result<(tokio::task::AbortHandle, EmergencyClient), SimulatorClientError> {
    let client = super::client_lifecycle16::connect(
        config,
        traces,
        remote_commands.clone(),
        Arc::clone(&state),
        Arc::clone(&reply_delay),
    )
    .await?;
    traces.push(TraceKind::Connected, OcppVersion::V1_6.websocket_protocol());
    let live_client = Arc::new(tokio::sync::Mutex::new(client.clone()));
    let emergency_client = EmergencyClient::V1_6(Arc::clone(&live_client));
    let worker = tokio::spawn(run_1_6(
        client,
        commands,
        traces.clone(),
        config.command_capacity,
        remote_receiver,
        state,
        config.clone(),
        remote_commands,
        reply_delay,
        live_client,
    ))
    .abort_handle();
    Ok((worker, emergency_client))
}

pub(super) async fn register_1_6_handlers(
    client: &ocpp_client::ocpp_1_6::OCPP1_6Client,
    traces: &TraceBuffer,
    remote_commands: mpsc::Sender<RemoteCommand>,
    connectors: Vec<u16>,
    state: Arc<Mutex<Ocpp16State>>,
    reply_delay: ReplyDelaySlot,
) {
    let start_traces = traces.clone();
    let commands = remote_commands.clone();
    let start_state = Arc::clone(&state);
    let start_delay = Arc::clone(&reply_delay);
    client
        .on_remote_start_transaction(move |request, _client| {
            let connector = request
                .connector_id
                .and_then(|value| u16::try_from(value).ok());
            let accepted = connector.is_none_or(|value| {
                let state = start_state.lock().expect("OCPP 1.6 state lock poisoned");
                connectors.contains(&value)
                    && !state
                        .active_transactions
                        .values()
                        .any(|active| *active == value)
                    && !state
                        .local
                        .as_ref()
                        .is_some_and(|local| local.connector_busy(value))
            });
            start_traces.push(
                TraceKind::RemoteStartReceived,
                if accepted { "accepted" } else { "rejected" },
            );
            let delayed_reply = take_reply_delay(&start_delay, RemoteCommandKind::StartTransaction);
            let payload = serde_json::to_value(&request).unwrap_or(serde_json::Value::Null);
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
                Ok(RemoteStartTransactionResponse {
                    status: if accepted {
                        RemoteStartTransactionResponseStatus::Accepted
                    } else {
                        RemoteStartTransactionResponseStatus::Rejected
                    },
                })
            }
        })
        .await;

    let stop_traces = traces.clone();
    let stop_delay = reply_delay;
    client
        .on_remote_stop_transaction(move |request, _client| {
            let state = state.lock().expect("OCPP 1.6 state lock poisoned");
            let accepted = state
                .active_transactions
                .contains_key(&request.transaction_id)
                || state
                    .local
                    .as_ref()
                    .is_some_and(|local| local.active_connector(request.transaction_id).is_some());
            drop(state);
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
                Ok(RemoteStopTransactionResponse {
                    status: if accepted {
                        RemoteStopTransactionResponseStatus::Accepted
                    } else {
                        RemoteStopTransactionResponseStatus::Rejected
                    },
                })
            }
        })
        .await;
}

pub(super) async fn register_2_0_1_reconnect(
    client: &ocpp_client::ocpp_2_0_1::OCPP2_0_1Client,
    traces: &TraceBuffer,
) {
    let traces = traces.clone();
    client
        .on_reconnect(move |_| {
            traces.push(TraceKind::Reconnected, "ocpp2.0.1");
            async {}
        })
        .await;
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_1_6(
    mut client: ocpp_client::ocpp_1_6::OCPP1_6Client,
    mut commands: mpsc::Receiver<Command>,
    traces: TraceBuffer,
    outstanding_capacity: usize,
    remote_commands: mpsc::Receiver<RemoteCommand>,
    state: Arc<Mutex<Ocpp16State>>,
    config: SimulatorClientConfig,
    remote_sender: mpsc::Sender<RemoteCommand>,
    reply_delay: ReplyDelaySlot,
    live_client: Arc<tokio::sync::Mutex<ocpp_client::ocpp_1_6::OCPP1_6Client>>,
) {
    let mut requests = JoinSet::new();
    let mut replay = JoinSet::new();
    let remote_commands = Arc::new(tokio::sync::Mutex::new(remote_commands));
    let mut reset_poll = tokio::time::interval(std::time::Duration::from_millis(20));
    loop {
        tokio::select! {
            _ = reset_poll.tick() => {
                let reset = state.lock().expect("OCPP 1.6 state lock").reset_reason.is_some();
                if reset {
                    requests.shutdown().await;
                    replay.shutdown().await;
                    if let Ok(recovered) = super::client_lifecycle16::reboot(&client, &config, &traces,
                        remote_sender.clone(), Arc::clone(&state), Arc::clone(&reply_delay)).await {
                        *live_client.lock().await = recovered.clone();
                        client = recovered;
                    } else {
                        traces.push(TraceKind::Failed, "native reset recovery failed");
                        break;
                    }
                }
                schedule_replay(&mut replay, &client, &state, &traces);
            }
            _ = replay.join_next(), if !replay.is_empty() => {}
            _ = requests.join_next(), if !requests.is_empty() => {}
            command = commands.recv(), if requests.len() < outstanding_capacity => match command {
                Some(Command::Heartbeat(result)) => {
                    let client = client.clone();
                    let traces = traces.clone();
                    requests.spawn(async move {
                        traces.push(TraceKind::HeartbeatSent, "Heartbeat");
                        let response = client.send_heartbeat(HeartbeatRequest16 {}).await
                            .map(|response| response.current_time.to_string())
                            .map_err(|error| SimulatorClientError::Protocol(error.to_string()));
                        record_result(&traces, &response);
                        let _ = result.send(response);
                    });
                }
                Some(Command::Call(call, result)) => {
                    let client = client.clone();
                    let traces = traces.clone();
                    let state = Arc::clone(&state);
                    requests.spawn(async move {
                        traces.push(TraceKind::ChargingCallSent, call.action.name());
                        let exchange = crate::client_exchange16::NativeExchange::capture(&state, &call);
                        let response = exchange.send(&client, &call).await;
                        let response = finish_call(&state, &call, response, &traces, exchange);
                        traces.push(if response.is_ok() { TraceKind::ChargingCallResult } else { TraceKind::Failed }, call.action.name());
                        let _ = result.send(response);
                    });
                }
                Some(Command::NextRemote(result)) => {
                    let remote_commands = Arc::clone(&remote_commands);
                    requests.spawn(async move {
                        let response = remote_commands.lock().await.recv().await.ok_or(SimulatorClientError::Stopped);
                        let _ = result.send(response);
                    });
                }
                Some(Command::LocalListConflict) => {
                    let client = client.clone();
                    let traces = traces.clone();
                    requests.spawn(async move {
                        crate::client_observation16::notify_conflict(&client, &traces).await;
                    });
                }
                Some(Command::Shutdown(result)) => {
                    requests.shutdown().await;
                    replay.shutdown().await;
                    let response = client.disconnect().await
                        .map_err(|error| SimulatorClientError::Protocol(error.to_string()));
                    traces.push(TraceKind::Stopped, "client disconnected");
                    state.lock().expect("OCPP 1.6 state lock").local = None;
                    let _ = result.send(response);
                    break;
                }
                None => { requests.shutdown().await; replay.shutdown().await; break; }
            }
        }
    }
    state.lock().expect("OCPP 1.6 state lock").local = None;
}

pub(crate) async fn send_1_6_call(
    client: &ocpp_client::ocpp_1_6::OCPP1_6Client,
    call: &SimulatorCall,
) -> Result<serde_json::Value, SimulatorClientError> {
    macro_rules! exchange {
        ($request:ty, $method:ident) => {{
            let request: $request = serde_json::from_value(call.payload.clone())
                .map_err(|error| SimulatorClientError::Protocol(error.to_string()))?;
            let response = client
                .$method(request)
                .await
                .map_err(|error| SimulatorClientError::Protocol(error.to_string()))?;
            serde_json::to_value(response)
                .map_err(|error| SimulatorClientError::Protocol(error.to_string()))
        }};
    }
    match call.action {
        SimulatorAction::BootNotification => {
            exchange!(BootNotificationRequest, send_boot_notification)
        }
        SimulatorAction::Authorize
        | SimulatorAction::StartTransaction
        | SimulatorAction::StopTransaction => {
            crate::local_authorization::wire::call(client, call.action, &call.payload).await
        }
        SimulatorAction::StatusNotification => {
            exchange!(StatusNotificationRequest, send_status_notification)
        }
        SimulatorAction::MeterValues => exchange!(MeterValuesRequest, send_meter_values),
    }
}

pub(crate) fn finish_call(
    state: &Arc<Mutex<Ocpp16State>>,
    call: &SimulatorCall,
    response: Result<serde_json::Value, SimulatorClientError>,
    traces: &TraceBuffer,
    exchange: crate::client_exchange16::NativeExchange,
) -> Result<serde_json::Value, SimulatorClientError> {
    match response {
        Ok(value) => {
            observe_local_authorization(state, call, &value, traces)?;
            update_ocpp16_state(state, call, &value, exchange, true)?;
            Ok(value)
        }
        Err(_) => Err(SimulatorClientError::Protocol(
            "native exchange failed".to_owned(),
        )),
    }
}

pub(crate) fn update_ocpp16_state(
    state: &Arc<Mutex<Ocpp16State>>,
    call: &SimulatorCall,
    response: &serde_json::Value,
    exchange: crate::client_exchange16::NativeExchange,
    request_replay: bool,
) -> Result<(), SimulatorClientError> {
    let mut state = state.lock().expect("OCPP 1.6 state lock poisoned");
    if state.socket_generation != exchange.generation {
        return Err(crate::client_exchange16::stale_exchange());
    }
    match call.action {
        SimulatorAction::BootNotification => {
            let accepted =
                response.get("status").and_then(serde_json::Value::as_str) == Some("Accepted");
            if !state.socket_connected
                || state.boot_accepted_generation != accepted.then_some(exchange.generation)
            {
                return Err(crate::client_exchange16::stale_exchange());
            }
            state.boot = Some(call.payload.clone());
            state.registered = accepted;
            state.replay_requested = accepted && request_replay;
        }
        SimulatorAction::StatusNotification | SimulatorAction::MeterValues => {
            if let Some(id) = call
                .payload
                .get("connectorId")
                .and_then(serde_json::Value::as_u64)
                .and_then(|id| u16::try_from(id).ok())
            {
                let values = if call.action == SimulatorAction::MeterValues {
                    &mut state.meters
                } else {
                    &mut state.status
                };
                values.insert(id, call.payload.clone());
            }
        }
        _ => {}
    }
    if call.action == SimulatorAction::StartTransaction
        && response
            .pointer("/idTagInfo/status")
            .and_then(serde_json::Value::as_str)
            == Some("Accepted")
        && let (Some(transaction_id), Some(connector_id)) = (
            response
                .get("transactionId")
                .and_then(serde_json::Value::as_i64),
            call.payload
                .get("connectorId")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u16::try_from(value).ok()),
        )
    {
        state
            .active_transactions
            .insert(transaction_id, connector_id);
    }
    if call.action == SimulatorAction::StopTransaction
        && let Some(transaction_id) = call
            .payload
            .get("transactionId")
            .and_then(serde_json::Value::as_i64)
    {
        state.active_transactions.remove(&transaction_id);
    }
    Ok(())
}

fn record_result(traces: &TraceBuffer, response: &Result<String, SimulatorClientError>) {
    match response {
        Ok(timestamp) => traces.push(TraceKind::HeartbeatResult, timestamp),
        Err(error) => traces.push(TraceKind::Failed, error.to_string()),
    }
}

pub(crate) fn schedule_replay(
    replay: &mut JoinSet<()>,
    client: &ocpp_client::ocpp_1_6::OCPP1_6Client,
    state: &Arc<Mutex<Ocpp16State>>,
    traces: &TraceBuffer,
) {
    if replay.is_empty() {
        let generation = {
            let mut state = state.lock().expect("OCPP 1.6 state lock");
            let requested = std::mem::take(&mut state.replay_requested);
            (requested
                && state.registered
                && state.socket_connected
                && state.boot_accepted_generation == Some(state.socket_generation))
            .then_some(state.socket_generation)
        };
        if let Some(generation) = generation {
            let client = client.clone();
            let state = Arc::clone(state);
            let traces = traces.clone();
            replay.spawn(async move {
                if super::client_lifecycle16::replay_pending(
                    client,
                    state,
                    traces.clone(),
                    generation,
                )
                .await
                .is_err()
                {
                    traces.push(TraceKind::Failed, "offline_replay_uncertain");
                }
            });
        }
    }
}
