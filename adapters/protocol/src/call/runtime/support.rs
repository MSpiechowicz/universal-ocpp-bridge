use super::{
    BTreeMap, CallSessionDiagnostic, FlowEvidence, FlowStage, Instant, PendingEntry,
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
            state.timed_out.push_back((id, correlation_id));
        }
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
