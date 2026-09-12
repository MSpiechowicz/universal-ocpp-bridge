//! Conservative linking of committed transaction evidence; never infer physical success from a reply.
use uob_contracts::{
    Command, CommandOperation, EventEnvelope, EventOrigin, ObservedCommandEffect,
    TransactionSnapshot, TransactionState,
};

/// Maps a durable station event to a separately observed effect of a command.
/// The host must read this event from the authoritative journal before passing the effect to
/// `CommandCoordinator::reconcile_observed_effect`. OCPP 1.6 has no remote-request ID in its
/// transaction notifications, so this is compatible evidence, never proof of unique causation.
/// Reset/Unlock may cause transaction completion; neither a stop nor reconnect proves a physical
/// reset/unlock. No synthetic completion is produced for either operation.
#[must_use]
pub fn transaction_effect<P>(
    command: &Command<P>,
    event: &EventEnvelope<TransactionSnapshot>,
) -> Option<ObservedCommandEffect> {
    if event.origin != EventOrigin::Station
        || event.observed_at < command.admitted_at
        || event.resource != event.payload.resource
        || event.resource.bridge_id != command.resource.bridge_id
        || event.resource.station_id != command.resource.station_id
        || (command.resource.resource.is_some() && event.resource != command.resource)
    {
        return None;
    }
    let native = event.payload.ocpp16.as_ref()?;
    let matches = match &command.operation {
        CommandOperation::Start {
            authorization_reference: Some(reference),
        } => {
            event.payload.state != TransactionState::Ended
                && event.event_type.as_str() == "transaction.started"
                && native.authorization_status == "Accepted"
                && native.identity_reference.as_ref() == Some(reference)
                && event.payload.started_at >= command.admitted_at
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
