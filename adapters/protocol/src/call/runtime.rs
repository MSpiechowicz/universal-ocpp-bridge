mod support;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Duration,
};
use support::{emit, expire_calls, finish_not_transmitted, retain_recent, uncertain_all};

use axum::extract::ws::Message;
use tokio::{
    sync::{mpsc, oneshot},
    time::{Instant, sleep_until},
};
use uob_application::{
    Application, FlowDiagnostics, FlowEvidence, FlowSpan, FlowStage, RuntimeResourceBudget,
    WorkClass,
};
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
        application.diagnostics().clone(),
        application
            .runtime_identity()
            .process_instance_id
            .as_str()
            .to_owned(),
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
    trace: FlowDiagnostics,
    correlation_prefix: String,
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
    trace: FlowDiagnostics,
    process_id: String,
    mut outbound: mpsc::Receiver<QueuedOutbound>,
    incoming: mpsc::Sender<IncomingCall>,
    replies: mpsc::Sender<QueuedReply>,
    mut reply_receiver: mpsc::Receiver<QueuedReply>,
    diagnostics: mpsc::Sender<CallSessionDiagnostic>,
    mut shutdown: oneshot::Receiver<()>,
) {
    static SESSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let session = SESSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut state = SessionState {
        trace,
        correlation_prefix: format!("{process_id}:{session}"),
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
                let sent = connection.send(Message::Text(reply.encoded.into())).await;
                reply.trace.emit(FlowStage::OcppSend, if sent.is_ok() { FlowEvidence::Completed } else { FlowEvidence::Uncertain });
                if sent.is_err() {
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
    let trace = state.span(Some(queued.request.correlation_id.clone()));
    let message_id = queued.request.message_id.clone();
    let correlation_id = queued.request.correlation_id.clone();
    if connection
        .send(Message::Text(queued.encoded.into()))
        .await
        .is_err()
    {
        trace.emit(FlowStage::OcppSend, FlowEvidence::Uncertain);
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
    trace.emit(FlowStage::OcppSend, FlowEvidence::Completed);
    state.pending.insert(
        message_id,
        PendingEntry {
            trace,
            result: queued.result,
            correlation_id,
            deadline: Instant::now() + configuration.response_timeout,
            _reservation: queued.reservation,
        },
    );
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
            state
                .span(None)
                .emit(FlowStage::Validation, FlowEvidence::Rejected);
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
    let correlation_id =
        CorrelationId::new(format!("ocpp:{}:{message_id}", state.correlation_prefix))
            .expect("correlation ID");
    let trace = state.span(Some(correlation_id.clone()));
    trace.emit(FlowStage::OcppReceive, FlowEvidence::Completed);
    if state.incoming_ids.contains(&message_id) || state.recent_incoming.contains(&message_id) {
        trace.emit(FlowStage::Deduplication, FlowEvidence::Duplicate);
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
            trace.emit(FlowStage::Validation, FlowEvidence::Rejected);
            send_error(connection, &message_id, error.call_error()).await;
            return;
        }
    };
    let Ok(reservation) = budget.try_reserve(WorkClass::ChargerRequest, bytes.len()) else {
        send_capacity_error(connection, &message_id, state.protocol).await;
        return;
    };
    trace.emit(FlowStage::Validation, FlowEvidence::Completed);
    let call = IncomingCall {
        trace: trace.clone(),
        call: decoded,
        correlation_id,
        responder: IncomingCallResponder {
            trace,
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
        entry
            .trace
            .emit(FlowStage::OcppReceive, FlowEvidence::Completed);
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
        state
            .span(Some(correlation_id.clone()))
            .emit(FlowStage::OcppReceive, FlowEvidence::Stale);
        emit(
            diagnostics,
            CallSessionDiagnostic::LateResponse {
                message_id,
                correlation_id,
            },
        );
    } else {
        state
            .span(None)
            .emit(FlowStage::OcppReceive, FlowEvidence::Uncorrelated);
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

impl SessionState {
    fn span(&self, correlation: Option<CorrelationId>) -> FlowSpan {
        self.trace.span(
            correlation,
            Some(uob_contracts::StationId::new(self.station_id.clone()).expect("station")),
            Some(self.protocol),
        )
    }
}
