//! Match only committed station journal events against durably admitted commands.
use std::{io, sync::Arc};

use uob_application::{
    CommandCoordinator, OperationalStore, PageLimit, StationEvent,
    remote_control::RemoteControlStore,
};
use uob_contracts::{EventEnvelope, EventId, EventOrigin, ResourceRef};
use uob_protocol_adapter::{v16, v201};

use super::{Clock, unavailable};
use crate::charging::{ChargingStore, commands::LiveCommands};

pub(super) async fn reconcile(
    store: &ChargingStore,
    commands: &Arc<LiveCommands>,
    station: &ResourceRef,
    event_id: EventId,
) -> io::Result<()> {
    let Some(event) = store
        .journal_event_by_id(event_id, station.clone())
        .await
        .map_err(|_| unavailable())?
    else {
        // A duplicate transaction notification reserves an ID but commits no second event.
        return Ok(());
    };
    if event.origin != EventOrigin::Station || event.provenance.is_some() {
        return Ok(());
    }
    let coordinator =
        CommandCoordinator::new(Arc::new(store.clone()), commands.clone(), Arc::new(Clock));
    let limit = PageLimit::new(100).map_err(|_| unavailable())?;
    let mut after = None;
    loop {
        let candidates = store
            .command_candidates(station.clone(), event.observed_at, after.take(), limit)
            .await
            .map_err(|_| unavailable())?;
        let count = candidates.len();
        for command in candidates {
            let effect = match &event.payload {
                StationEvent::StationSnapshot(snapshot) => {
                    let observed = with_payload(&event, snapshot.clone());
                    match &command.operation {
                        uob_contracts::CommandOperation::Ocpp(operation)
                            if operation.protocol == uob_contracts::ProtocolEdition::Ocpp201 =>
                        {
                            v201::availability::observed_effect(&command, &observed)
                        }
                        uob_contracts::CommandOperation::Ocpp(_) => {
                            v16::availability::observed_effect(&command, &observed)
                        }
                        _ => None,
                    }
                }
                StationEvent::Transaction(transaction) => {
                    let observed = with_payload(&event, transaction.clone());
                    match transaction
                        .protocol_state
                        .as_ref()
                        .map(|state| state.protocol)
                    {
                        Some(uob_contracts::ProtocolEdition::Ocpp201) => {
                            match store
                                .remote_control_evidence(command.request_id.clone())
                                .await
                                .map_err(|_| unavailable())?
                            {
                                Some(evidence) => {
                                    v201::remote_control::observation::transaction_effect(
                                        &command, &observed, &evidence,
                                    )
                                }
                                None => None,
                            }
                        }
                        _ => v16::remote_control::observation::transaction_effect(
                            &command, &observed,
                        ),
                    }
                }
                StationEvent::Invalidation { .. } => None,
            };
            if let Some(effect) = effect {
                coordinator
                    .reconcile_observed_effect(command.request_id.clone(), effect)
                    .await
                    .map_err(|_| unavailable())?;
            }
            after = Some(command.request_id);
        }
        if count < usize::from(limit.get()) {
            break;
        }
    }
    Ok(())
}

fn with_payload<T: Clone>(event: &EventEnvelope<StationEvent>, payload: T) -> EventEnvelope<T> {
    EventEnvelope {
        event_id: event.event_id.clone(),
        schema_version: event.schema_version,
        runtime: event.runtime.clone(),
        resource: event.resource.clone(),
        source_time: event.source_time,
        observed_at: event.observed_at,
        event_type: event.event_type.clone(),
        origin: event.origin.clone(),
        sequence: event.sequence,
        correlation_id: event.correlation_id.clone(),
        causation_id: event.causation_id.clone(),
        provenance: event.provenance.clone(),
        payload,
    }
}
