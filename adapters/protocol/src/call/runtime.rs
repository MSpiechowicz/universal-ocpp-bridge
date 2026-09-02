use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Duration,
};

use axum::extract::ws::Message;
use tokio::{
    sync::{mpsc, oneshot},
    time::{Instant, sleep_until},
};
use uob_application::{Application, RuntimeResourceBudget, WorkClass};
use uob_contracts::{CorrelationId, ProtocolEdition};

use super::{
    frame::{self, Frame},
    types::{
        CallSessionConfiguration, CallSessionConfigurationError, CallSessionDiagnostic,
        CallSessionHandle, CallSessionOutputs, CallSessionTask, IncomingCall, IncomingCallReceiver,
        IncomingCallResponder, PendingEntry, QueuedOutbound, QueuedReply, RemoteCallError,
        SessionCallOutcome, TransmissionUncertainReason,
    },
};
use crate::{OcppCallError, OcppErrorCode, StationConnection, v16, v201};

/// Starts the single socket owner and its bounded call lifecycle queues.
///
/// # Errors
///
/// Rejects invalid queue or deadline configuration before spawning.
pub fn spawn_call_session(
    connection: StationConnection,
    application: &Application,
    configuration: CallSessionConfiguration,
) -> Result<(CallSessionHandle, CallSessionOutputs, CallSessionTask), CallSessionConfigurationError>
{
    let configuration = configuration.validate(application)?;
    let budget = application.health().resources().clone();
    let (outbound_sender, outbound_receiver) = mpsc::channel(configuration.pending_call_capacity);
    let (incoming_sender, incoming_receiver) = mpsc::channel(configuration.incoming_call_capacity);
    let (reply_sender, reply_receiver) = mpsc::channel(configuration.incoming_call_capacity);
    let (diagnostic_sender, diagnostic_receiver) = mpsc::channel(configuration.diagnostic_capacity);
    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
    let actor_budget = budget.clone();
    let join = tokio::spawn(run_session(
        connection,
        configuration,
        actor_budget,
        outbound_receiver,
        incoming_sender,
        reply_sender,
        reply_receiver,
        diagnostic_sender,
        shutdown_receiver,
    ));
    Ok((
        CallSessionHandle {
            sender: outbound_sender,
            budget,
        },
        CallSessionOutputs {
            incoming: IncomingCallReceiver {
                receiver: incoming_receiver,
            },
            diagnostics: diagnostic_receiver,
        },
        CallSessionTask {
            shutdown: Some(shutdown_sender),
            join: Some(join),
        },
    ))
}

struct SessionState {
    protocol: ProtocolEdition,
    station_id: String,
    pending: BTreeMap<String, PendingEntry>,
    incoming_ids: BTreeSet<String>,
    recent_incoming: VecDeque<String>,
    timed_out: VecDeque<(String, CorrelationId)>,
    retired_outbound: VecDeque<String>,
    history_capacity: usize,
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    mut connection: StationConnection,
    configuration: CallSessionConfiguration,
    budget: RuntimeResourceBudget,
    mut outbound: mpsc::Receiver<QueuedOutbound>,
    incoming: mpsc::Sender<IncomingCall>,
    replies: mpsc::Sender<QueuedReply>,
    mut reply_receiver: mpsc::Receiver<QueuedReply>,
    diagnostics: mpsc::Sender<CallSessionDiagnostic>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut state = SessionState {
        protocol: connection.station().protocol,
        station_id: connection.station().station_id.as_str().to_owned(),
        pending: BTreeMap::new(),
        incoming_ids: BTreeSet::new(),
        recent_incoming: VecDeque::new(),
        timed_out: VecDeque::new(),
        retired_outbound: VecDeque::new(),
        history_capacity: configuration.diagnostic_capacity,
    };
    loop {
        let deadline = state
            .pending
            .values()
            .map(|entry| entry.deadline)
            .min()
            .unwrap_or_else(|| Instant::now() + Duration::from_hours(24));
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                uncertain_all(&mut state.pending, TransmissionUncertainReason::SessionStopped);
                let _ = connection.send(Message::Close(None)).await;
                return;
            }
            message = connection.receive() => {
                let Some(message) = message else {
                    uncertain_all(&mut state.pending, TransmissionUncertainReason::Disconnected);
                    return;
                };
                match message {
                    Ok(Message::Text(text)) => {
                        process_frame(
                            text.as_bytes(), &mut state, &mut connection, &budget, &incoming,
                            &replies, &diagnostics,
                        ).await;
                    }
                    Ok(Message::Close(_)) | Err(_) => {
                        uncertain_all(&mut state.pending, TransmissionUncertainReason::Disconnected);
                        return;
                    }
                    Ok(Message::Binary(_)) => {
                        emit(&diagnostics, CallSessionDiagnostic::MalformedFrame {
                            message_id: None,
                            field_path: "/",
                        });
                        uncertain_all(&mut state.pending, TransmissionUncertainReason::Disconnected);
                        let _ = connection.send(Message::Close(None)).await;
                        return;
                    }
                    Ok(Message::Ping(_) | Message::Pong(_)) => {}
                }
            }
            Some(queued) = outbound.recv() => {
                send_outbound(&mut connection, &mut state, queued, configuration).await;
            }
            Some(reply) = reply_receiver.recv() => {
                state.incoming_ids.remove(&reply.message_id);
                if connection.send(Message::Text(reply.encoded.into())).await.is_err() {
                    uncertain_all(&mut state.pending, TransmissionUncertainReason::Disconnected);
                    return;
                }
            }
            () = sleep_until(deadline), if !state.pending.is_empty() => {
                expire_calls(&mut state, configuration.diagnostic_capacity);
            }
            else => {
                uncertain_all(&mut state.pending, TransmissionUncertainReason::SessionStopped);
                return;
            }
        }
    }
}

async fn send_outbound(
    connection: &mut StationConnection,
    state: &mut SessionState,
    queued: QueuedOutbound,
    configuration: CallSessionConfiguration,
) {
    if state.pending.len() == configuration.pending_call_capacity {
        finish_not_transmitted(queued, "pending call capacity exhausted");
        return;
    }
    if state.pending.contains_key(&queued.request.message_id)
        || state
            .timed_out
            .iter()
            .any(|(id, _)| id == &queued.request.message_id)
        || state.retired_outbound.contains(&queued.request.message_id)
    {
        finish_not_transmitted(queued, "duplicate or retired message ID");
        return;
    }
    let message_id = queued.request.message_id.clone();
    let correlation_id = queued.request.correlation_id.clone();
    if connection
        .send(Message::Text(queued.encoded.into()))
        .await
        .is_err()
    {
        let _ = queued
            .result
            .send(SessionCallOutcome::TransmissionUncertain {
                reason: TransmissionUncertainReason::WriteFailed,
                correlation_id,
            });
        uncertain_all(
            &mut state.pending,
            TransmissionUncertainReason::Disconnected,
        );
        return;
    }
    state.pending.insert(
        message_id,
        PendingEntry {
            result: queued.result,
            correlation_id,
            deadline: Instant::now() + configuration.response_timeout,
            _reservation: queued.reservation,
        },
    );
}

fn expire_calls(state: &mut SessionState, history_capacity: usize) {
    let now = Instant::now();
    let expired = state
        .pending
        .iter()
        .filter(|(_, entry)| entry.deadline <= now)
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in expired {
        if let Some(entry) = state.pending.remove(&id) {
            let correlation_id = entry.correlation_id.clone();
            let _ = entry.result.send(SessionCallOutcome::TimedOut {
                correlation_id: correlation_id.clone(),
            });
            if state.timed_out.len() == history_capacity {
                state.timed_out.pop_front();
            }
            state.timed_out.push_back((id, correlation_id));
        }
    }
}

async fn process_frame(
    bytes: &[u8],
    state: &mut SessionState,
    connection: &mut StationConnection,
    budget: &RuntimeResourceBudget,
    incoming: &mpsc::Sender<IncomingCall>,
    replies: &mpsc::Sender<QueuedReply>,
    diagnostics: &mpsc::Sender<CallSessionDiagnostic>,
) {
    let parsed = match frame::decode(bytes) {
        Ok(frame) => frame,
        Err(error) => {
            emit(
                diagnostics,
                CallSessionDiagnostic::MalformedFrame {
                    message_id: error.message_id.clone(),
                    field_path: error.field_path,
                },
            );
            if let Some(message_id) = error.message_id {
                let error = frame::protocol_error(
                    state.protocol,
                    "OCPP frame violates protocol shape",
                    Some(error.field_path),
                );
                send_error(connection, &message_id, error).await;
            }
            return;
        }
    };
    match parsed {
        Frame::Call { message_id, bytes } => {
            process_incoming_call(
                message_id,
                bytes,
                state,
                connection,
                budget,
                incoming,
                replies,
                diagnostics,
            )
            .await;
        }
        Frame::Result {
            message_id,
            payload,
        } => finish_response(message_id, state, diagnostics, |correlation_id| {
            SessionCallOutcome::Result {
                payload,
                correlation_id,
            }
        }),
        Frame::Error {
            message_id,
            code,
            _description: _,
            details,
        } => {
            let code = frame::safe_remote_code(&code).to_owned();
            let field_path = frame::safe_field_path(&details);
            finish_response(message_id, state, diagnostics, |correlation_id| {
                SessionCallOutcome::Error {
                    error: RemoteCallError { code, field_path },
                    correlation_id,
                }
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_incoming_call(
    message_id: String,
    bytes: Vec<u8>,
    state: &mut SessionState,
    connection: &mut StationConnection,
    budget: &RuntimeResourceBudget,
    incoming: &mpsc::Sender<IncomingCall>,
    replies: &mpsc::Sender<QueuedReply>,
    diagnostics: &mpsc::Sender<CallSessionDiagnostic>,
) {
    if state.incoming_ids.contains(&message_id) || state.recent_incoming.contains(&message_id) {
        emit(
            diagnostics,
            CallSessionDiagnostic::DuplicateIncomingCall {
                message_id: message_id.clone(),
            },
        );
        send_error(
            connection,
            &message_id,
            OcppCallError {
                protocol: state.protocol,
                code: OcppErrorCode::OccurrenceConstraintViolation,
                description: "duplicate OCPP CALL message ID",
                field_path: Some("/1"),
            },
        )
        .await;
        return;
    }
    let decoded = match state.protocol {
        ProtocolEdition::Ocpp16j => v16::decode_call(&bytes),
        ProtocolEdition::Ocpp201 => v201::decode_call(&bytes),
    };
    let decoded = match decoded {
        Ok(decoded) => decoded,
        Err(error) => {
            send_error(connection, &message_id, error.call_error()).await;
            return;
        }
    };
    let Ok(reservation) = budget.try_reserve(WorkClass::ChargerRequest, bytes.len()) else {
        send_capacity_error(connection, &message_id, state.protocol).await;
        return;
    };
    let correlation_id = CorrelationId::new(format!("ocpp:{}:{message_id}", state.station_id))
        .expect("station and nonempty OCPP message ID form a correlation ID");
    let call = IncomingCall {
        call: decoded,
        correlation_id,
        responder: IncomingCallResponder {
            message_id: message_id.clone(),
            protocol: state.protocol,
            sender: Some(replies.clone()),
            budget: budget.clone(),
        },
        _reservation: reservation,
    };
    match incoming.try_send(call) {
        Ok(()) => {
            state.incoming_ids.insert(message_id.clone());
            retain_recent(
                &mut state.recent_incoming,
                message_id,
                state.history_capacity,
            );
        }
        Err(error) => {
            let mut rejected = error.into_inner();
            rejected.responder.sender = None;
            send_capacity_error(connection, &message_id, state.protocol).await;
        }
    }
}

fn finish_response(
    message_id: String,
    state: &mut SessionState,
    diagnostics: &mpsc::Sender<CallSessionDiagnostic>,
    outcome: impl FnOnce(CorrelationId) -> SessionCallOutcome,
) {
    if let Some(entry) = state.pending.remove(&message_id) {
        let result = outcome(entry.correlation_id);
        let _ = entry.result.send(result);
        retain_recent(
            &mut state.retired_outbound,
            message_id,
            state.history_capacity,
        );
    } else if let Some(index) = state.timed_out.iter().position(|(id, _)| id == &message_id) {
        let (_, correlation_id) = state
            .timed_out
            .remove(index)
            .expect("located late response");
        retain_recent(
            &mut state.retired_outbound,
            message_id.clone(),
            state.history_capacity,
        );
        emit(
            diagnostics,
            CallSessionDiagnostic::LateResponse {
                message_id,
                correlation_id,
            },
        );
    } else {
        emit(
            diagnostics,
            CallSessionDiagnostic::UnmatchedResponse { message_id },
        );
    }
}

async fn send_capacity_error(
    connection: &mut StationConnection,
    message_id: &str,
    protocol: ProtocolEdition,
) {
    send_error(
        connection,
        message_id,
        OcppCallError {
            protocol,
            code: OcppErrorCode::InternalError,
            description: "charger request queue unavailable",
            field_path: None,
        },
    )
    .await;
}

async fn send_error(connection: &mut StationConnection, message_id: &str, error: OcppCallError) {
    let _ = connection
        .send(Message::Text(frame::error(message_id, error).into()))
        .await;
}

fn finish_not_transmitted(queued: QueuedOutbound, reason: &'static str) {
    let _ = queued.result.send(SessionCallOutcome::NotTransmitted {
        reason,
        correlation_id: queued.request.correlation_id,
    });
}

fn uncertain_all(
    pending: &mut BTreeMap<String, PendingEntry>,
    reason: TransmissionUncertainReason,
) {
    for (_, entry) in std::mem::take(pending) {
        let _ = entry
            .result
            .send(SessionCallOutcome::TransmissionUncertain {
                reason,
                correlation_id: entry.correlation_id,
            });
    }
}

fn emit(sender: &mpsc::Sender<CallSessionDiagnostic>, event: CallSessionDiagnostic) {
    let _ = sender.try_send(event);
}

fn retain_recent(history: &mut VecDeque<String>, message_id: String, capacity: usize) {
    if history.len() == capacity {
        history.pop_front();
    }
    history.push_back(message_id);
}
