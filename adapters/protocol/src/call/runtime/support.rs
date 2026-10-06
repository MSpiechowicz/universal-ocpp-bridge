use super::{
    BTreeMap, CallSessionDiagnostic, CorrelationId, FlowEvidence, FlowStage, Instant, PendingEntry,
    QueuedOutbound, SessionCallOutcome, SessionState, TransmissionUncertainReason, VecDeque, mpsc,
};

pub(super) fn expire_calls(state: &mut SessionState, history_capacity: usize) {
    let now = Instant::now();
    let expired = state
        .pending
        .iter()
        .filter(|(_, entry)| entry.deadline <= now)
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in expired {
        if let Some(entry) = state.pending.remove(&id) {
            entry
                .trace
                .emit(FlowStage::ProtocolResponse, FlowEvidence::Uncertain);
            let correlation_id = entry.correlation_id.clone();
            let _ = entry.result.send(SessionCallOutcome::TimedOut {
                correlation_id: correlation_id.clone(),
            });
            if state.timed_out.len() == history_capacity {
                state.timed_out.pop_front();
            }
            state
                .timed_out
                .push_back((id, correlation_id, entry.reservation_action));
        }
    }
}

pub(super) fn finish_response(
    message_id: String,
    state: &mut SessionState,
    diagnostics: &mpsc::Sender<CallSessionDiagnostic>,
    reservation: uob_application::RuntimeReservation,
    outcome: impl FnOnce(CorrelationId) -> SessionCallOutcome,
) {
    if let Some(entry) = state.pending.remove(&message_id) {
        entry
            .trace
            .emit(FlowStage::OcppReceive, FlowEvidence::Completed);
        if let Some(sender) = entry.response_reservation {
            let _ = sender.send(reservation);
        }
        let result = outcome(entry.correlation_id);
        if let Some(action) = entry.reservation_action
            && !reservation_status(state.protocol, action, &result)
        {
            if state.timed_out.len() == state.history_capacity {
                state.timed_out.pop_front();
            }
            let correlation_id = match &result {
                SessionCallOutcome::Result { correlation_id, .. }
                | SessionCallOutcome::Error { correlation_id, .. }
                | SessionCallOutcome::TimedOut { correlation_id }
                | SessionCallOutcome::NotTransmitted { correlation_id, .. }
                | SessionCallOutcome::TransmissionUncertain { correlation_id, .. } => {
                    correlation_id.clone()
                }
            };
            state
                .timed_out
                .push_back((message_id.clone(), correlation_id, Some(action)));
        }
        let _ = entry.result.send(result);
        retain_recent(
            &mut state.retired_outbound,
            message_id,
            state.history_capacity,
        );
    } else if let Some(index) = state
        .timed_out
        .iter()
        .position(|(id, _, _)| id == &message_id)
    {
        let correlation_id = state.timed_out[index].1.clone();
        let reservation_action = state.timed_out[index].2;
        retain_recent(
            &mut state.retired_outbound,
            message_id.clone(),
            state.history_capacity,
        );
        state
            .span(Some(correlation_id.clone()))
            .emit(FlowStage::OcppReceive, FlowEvidence::Stale);
        if let Some(action) = reservation_action {
            if let Some(diagnostic) = late_reservation(
                state.protocol,
                action,
                &outcome(correlation_id.clone()),
                &message_id,
                &correlation_id,
            ) {
                let _ = state.timed_out.remove(index);
                emit(diagnostics, diagnostic);
                return;
            }
        } else {
            let _ = state.timed_out.remove(index);
        }
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

fn reservation_status(
    protocol: uob_contracts::ProtocolEdition,
    action: &str,
    outcome: &SessionCallOutcome,
) -> bool {
    match protocol {
        uob_contracts::ProtocolEdition::Ocpp16j => reservation_status_16(action, outcome).is_some(),
        uob_contracts::ProtocolEdition::Ocpp201 => {
            reservation_status_201(action, outcome).is_some()
        }
    }
}
fn late_reservation(
    protocol: uob_contracts::ProtocolEdition,
    action: &'static str,
    outcome: &SessionCallOutcome,
    message_id: &str,
    correlation_id: &CorrelationId,
) -> Option<CallSessionDiagnostic> {
    let (message_id, correlation_id) = (message_id.to_owned(), correlation_id.clone());
    Some(match protocol {
        uob_contracts::ProtocolEdition::Ocpp16j => {
            CallSessionDiagnostic::LateReservationResponse16 {
                message_id,
                correlation_id,
                action,
                status: reservation_status_16(action, outcome)?,
            }
        }
        uob_contracts::ProtocolEdition::Ocpp201 => {
            CallSessionDiagnostic::LateReservationResponse201 {
                message_id,
                correlation_id,
                action,
                status: reservation_status_201(action, outcome)?,
            }
        }
    })
}
fn reservation_status_201(
    action: &str,
    outcome: &SessionCallOutcome,
) -> Option<uob_contracts::ReserveNowStatus201> {
    let SessionCallOutcome::Result { payload, .. } = outcome else {
        return None;
    };
    crate::v201::remote_control::reservation_native_status_201(action, payload)
}
fn reservation_status_16(
    action: &str,
    outcome: &SessionCallOutcome,
) -> Option<uob_contracts::ReserveNowStatus16> {
    use uob_contracts::ReserveNowStatus16 as Status;
    let SessionCallOutcome::Result { payload, .. } = outcome else {
        return None;
    };
    if !payload
        .as_object()
        .is_some_and(|o| o.len() == 1 && o.contains_key("status"))
    {
        return None;
    }
    match payload["status"].as_str()? {
        "Accepted" => Some(Status::Accepted),
        "Rejected" => Some(Status::Rejected),
        "Faulted" if action == "ReserveNow" => Some(Status::Faulted),
        "Occupied" if action == "ReserveNow" => Some(Status::Occupied),
        "Unavailable" if action == "ReserveNow" => Some(Status::Unavailable),
        _ => None,
    }
}

pub(super) fn finish_not_transmitted(queued: QueuedOutbound, reason: &'static str) {
    let _ = queued.result.send(SessionCallOutcome::NotTransmitted {
        reason,
        correlation_id: queued.request.correlation_id,
    });
}

pub(super) fn uncertain_all(
    pending: &mut BTreeMap<String, PendingEntry>,
    reason: TransmissionUncertainReason,
) {
    for (_, entry) in std::mem::take(pending) {
        entry
            .trace
            .emit(FlowStage::ProtocolResponse, FlowEvidence::Uncertain);
        let _ = entry
            .result
            .send(SessionCallOutcome::TransmissionUncertain {
                reason,
                correlation_id: entry.correlation_id,
            });
    }
}

pub(super) fn emit(sender: &mpsc::Sender<CallSessionDiagnostic>, event: CallSessionDiagnostic) {
    let _ = sender.try_send(event);
}

pub(super) fn retain_recent(history: &mut VecDeque<String>, message_id: String, capacity: usize) {
    if history.len() == capacity {
        history.pop_front();
    }
    history.push_back(message_id);
}
