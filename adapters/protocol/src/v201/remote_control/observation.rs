//! Link authoritative journal observations using persisted native correlation, never reply alone.
use uob_application::remote_control::RemoteControlEvidence;
use uob_contracts::{
    Command, CommandOperation, EventEnvelope, EventOrigin, ObservedCommandEffect, ProtocolEdition,
    TransactionSnapshot, TransactionState,
};

/// The caller reads both evidence and event from durable storage, then reconciles through the
/// normal coordinator after dispatch completes. Reset/unlock never infer physical success.
#[must_use]
pub fn transaction_effect<P>(
    command: &Command<P>,
    event: &EventEnvelope<TransactionSnapshot>,
    evidence: &RemoteControlEvidence,
) -> Option<ObservedCommandEffect> {
    if event.origin != EventOrigin::Station
        || event.observed_at < command.admitted_at
        || event.resource != event.payload.resource
        || !super::mapping::covers(&command.resource, &event.resource)
    {
        return None;
    }
    let native = event
        .payload
        .protocol_state
        .as_ref()
        .filter(|s| s.protocol == ProtocolEdition::Ocpp201)?;
    let matches = match &command.operation {
        CommandOperation::Start { .. } => {
            evidence.remote_start_id.is_some()
                && evidence.remote_start_id == native.remote_start_id
                && evidence.response_status.as_deref() != Some("Rejected")
                && evidence
                    .native_transaction_id
                    .as_ref()
                    .is_none_or(|id| id == &native.native_transaction_id)
                && native.last_trigger_reason == "RemoteStart"
                && native.last_event_at >= command.admitted_at
                && event.payload.state != TransactionState::Ended
                && matches!(
                    event.event_type.as_str(),
                    "transaction.started" | "transaction.updated"
                )
        }
        CommandOperation::Stop { transaction_id } => {
            event.payload.state == TransactionState::Ended
                && event.event_type.as_str() == "transaction.ended"
                && event
                    .payload
                    .ended_at
                    .is_some_and(|time| time >= command.admitted_at)
                && &event.payload.transaction_id == transaction_id
        }
        _ => false,
    };
    matches.then(|| ObservedCommandEffect {
        event_id: event.event_id.clone(),
        event_type: event.event_type.clone(),
        observed_at: event.observed_at,
    })
}
