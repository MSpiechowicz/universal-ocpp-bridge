use super::super::types::TransmissionUncertainReason;
use super::{
    CallSessionConfiguration, PendingEntry, QueuedOutbound, QueuedWire, SessionCallOutcome,
    SessionState, finish_not_transmitted, uncertain_all,
};
use crate::StationConnection;
use axum::extract::ws::Message;
use tokio::time::Instant;
use uob_application::{FlowEvidence, FlowStage};
pub(super) async fn send_outbound(
    connection: &mut StationConnection,
    state: &mut SessionState,
    queued: QueuedOutbound,
    configuration: CallSessionConfiguration,
) {
    if let Some(reason) = unavailable_before_send(state, &queued, configuration) {
        finish_not_transmitted(queued, reason);
        return;
    }
    let trace = state.span(Some(queued.request.correlation_id.clone()));
    let message_id = queued.request.message_id.clone();
    let correlation_id = queued.request.correlation_id.clone();
    let sent = if let QueuedWire::Configuration201(deferred) = &queued.wire {
        let Some(sent) = deferred.send_at_boundary(connection, &message_id).await else {
            finish_not_transmitted(
                queued,
                "protected configuration reference unavailable before socket send",
            );
            return;
        };
        sent
    } else if let QueuedWire::LocalAuthorization16(deferred) = &queued.wire {
        let Some(sent) = deferred.send_at_boundary(connection, &message_id).await else {
            finish_not_transmitted(
                queued,
                "protected local authorization reference unavailable before socket send",
            );
            return;
        };
        sent
    } else if let QueuedWire::LocalAuthorization201(deferred) = &queued.wire {
        let Some(sent) = deferred.send_at_boundary(connection, &message_id).await else {
            finish_not_transmitted(
                queued,
                "protected local authorization authority unavailable before socket send",
            );
            return;
        };
        sent
    } else {
        // The existing 1.6 resolver remains synchronous immediately before initiating send.
        let deferred_encoded = match &queued.wire {
            QueuedWire::Ready(_) => None,
            QueuedWire::Configuration(deferred) => {
                let Some(encoded) = deferred.encode_at_send(&message_id) else {
                    finish_not_transmitted(
                        queued,
                        "protected configuration reference unavailable before socket send",
                    );
                    return;
                };
                Some(encoded)
            }
            QueuedWire::Configuration201(_)
            | QueuedWire::LocalAuthorization16(_)
            | QueuedWire::LocalAuthorization201(_) => {
                unreachable!("handled above")
            }
        };
        let encoded = match queued.wire {
            QueuedWire::Ready(encoded) => encoded,
            QueuedWire::Configuration(_) => deferred_encoded.expect("checked above"),
            QueuedWire::Configuration201(_)
            | QueuedWire::LocalAuthorization16(_)
            | QueuedWire::LocalAuthorization201(_) => {
                unreachable!("handled above")
            }
        };
        if let Some(dispatched) = queued.dispatched {
            let _ = dispatched.send(Instant::now());
        }
        connection.send(Message::Text(encoded.into())).await
    };
    if sent.is_err() {
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
            response_reservation: queued.response_reservation,
        },
    );
}

fn unavailable_before_send(
    state: &SessionState,
    queued: &QueuedOutbound,
    configuration: CallSessionConfiguration,
) -> Option<&'static str> {
    if queued
        .send_before
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return Some("command expired before socket send");
    }
    if state.pending.len() == configuration.pending_call_capacity {
        return Some("pending call capacity exhausted");
    }
    if state.pending.contains_key(&queued.request.message_id)
        || state
            .timed_out
            .iter()
            .any(|(id, _)| id == &queued.request.message_id)
        || state.retired_outbound.contains(&queued.request.message_id)
    {
        return Some("duplicate or retired message ID");
    }
    None
}
