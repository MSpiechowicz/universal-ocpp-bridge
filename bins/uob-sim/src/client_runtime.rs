use std::sync::{Arc, Mutex};

use ocpp_client::ocpp_types::v16::common::{
    RemoteStartTransactionResponseStatus, RemoteStopTransactionResponseStatus, ResetResponseStatus,
};
use ocpp_client::ocpp_types::v16::{
    AuthorizeRequest, BootNotificationRequest, HeartbeatRequest as HeartbeatRequest16,
    MeterValuesRequest, RemoteStartTransactionResponse, RemoteStopTransactionResponse,
    ResetResponse as ResetResponse16, StartTransactionRequest, StatusNotificationRequest,
    StopTransactionRequest,
};
use ocpp_client::ocpp_types::v201::common::ResetStatusEnum;
use ocpp_client::ocpp_types::v201::{
    HeartbeatRequest as HeartbeatRequest201, ResetResponse as ResetResponse201,
};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::{
    Command, Ocpp16State, RemoteCommand, RemoteCommandKind, SimulatorAction, SimulatorCall,
    SimulatorClientError, TraceBuffer, TraceKind,
};

pub(super) async fn register_1_6_handlers(
    client: &ocpp_client::ocpp_1_6::OCPP1_6Client,
    traces: &TraceBuffer,
    remote_commands: mpsc::Sender<RemoteCommand>,
    connectors: Vec<u16>,
    state: Arc<Mutex<Ocpp16State>>,
) {
    let reset_traces = traces.clone();
    client
        .on_reset(move |_request, _client| {
            reset_traces.push(TraceKind::ResetReceived, "accepted");
            async move {
                Ok(ResetResponse16 {
                    status: ResetResponseStatus::Accepted,
                })
            }
        })
        .await;

    let start_traces = traces.clone();
    let commands = remote_commands.clone();
    let start_state = Arc::clone(&state);
    client
        .on_remote_start_transaction(move |request, _client| {
            let connector = request
                .connector_id
                .and_then(|value| u16::try_from(value).ok());
            let accepted = connector.is_none_or(|value| {
                connectors.contains(&value)
                    && !start_state
                        .lock()
                        .expect("OCPP 1.6 state lock poisoned")
                        .active_transactions
                        .values()
                        .any(|active| *active == value)
            });
            start_traces.push(
                TraceKind::RemoteStartReceived,
                if accepted { "accepted" } else { "rejected" },
            );
            let payload = serde_json::to_value(&request).unwrap_or(serde_json::Value::Null);
            let _ = commands.try_send(RemoteCommand {
                kind: RemoteCommandKind::StartTransaction,
                payload,
                accepted,
            });
            async move {
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
    client
        .on_remote_stop_transaction(move |request, _client| {
            let accepted = state
                .lock()
                .expect("OCPP 1.6 state lock poisoned")
                .active_transactions
                .contains_key(&request.transaction_id);
            stop_traces.push(
                TraceKind::RemoteStopReceived,
                if accepted { "accepted" } else { "rejected" },
            );
            let payload = serde_json::to_value(&request).unwrap_or(serde_json::Value::Null);
            let _ = remote_commands.try_send(RemoteCommand {
                kind: RemoteCommandKind::StopTransaction,
                payload,
                accepted,
            });
            async move {
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

pub(super) async fn register_2_0_1_handlers(
    client: &ocpp_client::ocpp_2_0_1::OCPP2_0_1Client,
    traces: &TraceBuffer,
) {
    let traces = traces.clone();
    client
        .on_reset(move |_request, _client| {
            traces.push(TraceKind::ResetReceived, "accepted");
            async move {
                Ok(ResetResponse201 {
                    custom_data: None,
                    status: ResetStatusEnum::Accepted,
                    status_info: None,
                })
            }
        })
        .await;
}

pub(super) async fn register_1_6_reconnect(
    client: &ocpp_client::ocpp_1_6::OCPP1_6Client,
    traces: &TraceBuffer,
) {
    let traces = traces.clone();
    client
        .on_reconnect(move |_| {
            traces.push(TraceKind::Reconnected, "ocpp1.6");
            async {}
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

pub(super) async fn run_1_6(
    client: ocpp_client::ocpp_1_6::OCPP1_6Client,
    mut commands: mpsc::Receiver<Command>,
    traces: TraceBuffer,
    outstanding_capacity: usize,
    mut remote_commands: mpsc::Receiver<RemoteCommand>,
    state: Arc<Mutex<Ocpp16State>>,
) {
    let mut requests = JoinSet::new();
    loop {
        tokio::select! {
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
                        let response = send_1_6_call(&client, &call).await;
                        if let Ok(value) = &response { update_ocpp16_state(&state, &call, value); }
                        traces.push(if response.is_ok() { TraceKind::ChargingCallResult } else { TraceKind::Failed }, call.action.name());
                        let _ = result.send(response);
                    });
                }
                Some(Command::NextRemote(result)) => {
                    let response = remote_commands.recv().await.ok_or(SimulatorClientError::Stopped);
                    let _ = result.send(response);
                }
                Some(Command::Shutdown(result)) => {
                    requests.shutdown().await;
                    let response = client.disconnect().await
                        .map_err(|error| SimulatorClientError::Protocol(error.to_string()));
                    traces.push(TraceKind::Stopped, "client disconnected");
                    let _ = result.send(response);
                    break;
                }
                None => { requests.shutdown().await; break; }
            }
        }
    }
}

pub(super) async fn run_2_0_1(
    client: ocpp_client::ocpp_2_0_1::OCPP2_0_1Client,
    mut commands: mpsc::Receiver<Command>,
    traces: TraceBuffer,
    outstanding_capacity: usize,
) {
    let mut requests = JoinSet::new();
    loop {
        tokio::select! {
            _ = requests.join_next(), if !requests.is_empty() => {}
            command = commands.recv(), if requests.len() < outstanding_capacity => match command {
                Some(Command::Heartbeat(result)) => {
                    let client = client.clone();
                    let traces = traces.clone();
                    requests.spawn(async move {
                        traces.push(TraceKind::HeartbeatSent, "Heartbeat");
                        let response = client.send_heartbeat(HeartbeatRequest201 { custom_data: None }).await
                            .map(|response| response.current_time.to_string())
                            .map_err(|error| SimulatorClientError::Protocol(error.to_string()));
                        record_result(&traces, &response);
                        let _ = result.send(response);
                    });
                }
                Some(Command::Call(_, result)) => { let _ = result.send(Err(SimulatorClientError::Protocol("OCPP 1.6 charging call used with an OCPP 2.0.1 station".to_owned()))); }
                Some(Command::NextRemote(result)) => { let _ = result.send(Err(SimulatorClientError::Protocol("OCPP 1.6 remote command used with an OCPP 2.0.1 station".to_owned()))); }
                Some(Command::Shutdown(result)) => {
                    requests.shutdown().await;
                    let response = client.disconnect().await.map_err(|error| SimulatorClientError::Protocol(error.to_string()));
                    traces.push(TraceKind::Stopped, "client disconnected");
                    let _ = result.send(response);
                    break;
                }
                None => { requests.shutdown().await; break; }
            }
        }
    }
}

async fn send_1_6_call(
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
        SimulatorAction::Authorize => exchange!(AuthorizeRequest, send_authorize),
        SimulatorAction::StatusNotification => {
            exchange!(StatusNotificationRequest, send_status_notification)
        }
        SimulatorAction::StartTransaction => {
            exchange!(StartTransactionRequest, send_start_transaction)
        }
        SimulatorAction::MeterValues => exchange!(MeterValuesRequest, send_meter_values),
        SimulatorAction::StopTransaction => {
            exchange!(StopTransactionRequest, send_stop_transaction)
        }
    }
}

fn update_ocpp16_state(
    state: &Arc<Mutex<Ocpp16State>>,
    call: &SimulatorCall,
    response: &serde_json::Value,
) {
    let mut state = state.lock().expect("OCPP 1.6 state lock poisoned");
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
}

fn record_result(traces: &TraceBuffer, response: &Result<String, SimulatorClientError>) {
    match response {
        Ok(timestamp) => traces.push(TraceKind::HeartbeatResult, timestamp),
        Err(error) => traces.push(TraceKind::Failed, error.to_string()),
    }
}
