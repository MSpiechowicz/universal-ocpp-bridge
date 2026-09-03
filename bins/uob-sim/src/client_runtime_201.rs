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
    Command, Ocpp201State, OcppVersion, RemoteCommand, RemoteCommandKind, SimulatorAction,
    SimulatorCall, SimulatorClientError, TraceBuffer, TraceKind,
};

pub(super) async fn register_handlers(
    client: &ocpp_client::ocpp_2_0_1::OCPP2_0_1Client,
    traces: &TraceBuffer,
    remote_commands: mpsc::Sender<RemoteCommand>,
    resources: Vec<(u16, u16)>,
    state: Arc<Mutex<Ocpp201State>>,
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
            let _ = commands.try_send(RemoteCommand {
                kind: RemoteCommandKind::StartTransaction,
                payload,
                accepted,
            });
            async move {
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
            let _ = remote_commands.try_send(RemoteCommand {
                kind: RemoteCommandKind::StopTransaction,
                payload,
                accepted,
            });
            async move {
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
    loop {
        tokio::select! {
            _ = requests.join_next(), if !requests.is_empty() => {}
            command = commands.recv(), if requests.len() < outstanding_capacity => match command {
                Some(Command::Heartbeat(result)) => {
                    let client = client.clone();
                    let traces = traces.clone();
                    requests.spawn(async move {
                        traces.push(TraceKind::HeartbeatSent, "Heartbeat");
                        let response = client.send_heartbeat(HeartbeatRequest { custom_data: None }).await
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
                        let wire_name = call.action.wire_name(OcppVersion::V2_0_1);
                        traces.push(TraceKind::ChargingCallSent, wire_name);
                        let response = Box::pin(send_call(&client, &call)).await;
                        if let Ok(value) = &response { update_state(&state, &call, value); }
                        traces.push(if response.is_ok() { TraceKind::ChargingCallResult } else { TraceKind::Failed }, wire_name);
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

async fn send_call(
    client: &ocpp_client::ocpp_2_0_1::OCPP2_0_1Client,
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
    let Some(transaction_id) = call
        .payload
        .pointer("/transactionInfo/transactionId")
        .and_then(serde_json::Value::as_str)
    else {
        return;
    };
    let mut state = state.lock().expect("OCPP 2.0.1 state lock poisoned");
    match call
        .payload
        .get("eventType")
        .and_then(serde_json::Value::as_str)
    {
        Some("Started")
            if response
                .pointer("/idTokenInfo/status")
                .and_then(serde_json::Value::as_str)
                == Some("Accepted") =>
        {
            if let (Some(evse), Some(connector)) = (
                unsigned_id(&call.payload, "/evse/id"),
                unsigned_id(&call.payload, "/evse/connectorId"),
            ) {
                state
                    .active_transactions
                    .insert(transaction_id.to_owned(), (evse, connector));
            }
        }
        Some("Ended") => {
            state.active_transactions.remove(transaction_id);
        }
        _ => {}
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
        Err(error) => traces.push(TraceKind::Failed, error.to_string()),
    }
}
