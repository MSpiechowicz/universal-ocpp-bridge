//! Explicit station-origin trigger evidence and bounded result reconciliation.
use std::{io, sync::Arc};

use uob_application::{AtomicStoreWrite, CommandCoordinator, OperationalStore, PageLimit};
use uob_contracts::{
    EventEnvelope, EventOrigin, EventType, ResourceRef, ServiceIdentity, StationEvent,
    TriggerMessageClass, UtcTimestamp,
};

use super::{Clock, event_identity, unavailable};
use crate::charging::{ChargingStore, commands::LiveCommands};

pub(super) async fn marker(
    store: &ChargingStore,
    station: &ResourceRef,
    identity: &ServiceIdentity,
    class: TriggerMessageClass,
    connector_id: Option<u32>,
    status: Option<&str>,
    now: UtcTimestamp,
) -> io::Result<Option<EventEnvelope<StationEvent>>> {
    if !store
        .trigger_pending_for_station(station.clone(), class, now)
        .await
        .map_err(|_| unavailable())?
    {
        return Ok(None);
    }

    let (sequence, event_id) = event_identity(store, identity).await?;
    Ok(Some(EventEnvelope {
        event_id,
        schema_version: uob_contracts::ContractVersion::V1_INITIAL,
        runtime: identity.runtime.clone(),
        resource: station.clone(),
        source_time: None,
        observed_at: now,
        event_type: EventType::new("station.trigger.notification").map_err(|_| unavailable())?,
        origin: EventOrigin::Station,
        sequence,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StationEvent::TriggerNotification {
            station_snapshot_invalidated: station.station_id.clone(),
            class,
            connector_id,
            status: status.map(str::to_owned),
        },
    }))
}
/// A diagnostic/firmware status has no workflow snapshot to replace; keep only its bounded evidence.
pub(super) async fn commit_status(
    store: &ChargingStore,
    marker: EventEnvelope<StationEvent>,
) -> io::Result<()> {
    let mut write = AtomicStoreWrite::empty();
    write.journal_events.push(marker);
    store.write_atomic(write).await.map_err(|_| unavailable())?;
    Ok(())
}

/// Process one bounded page per tick and restart from the beginning after the final page.
pub(super) async fn sweep(
    store: &ChargingStore,
    commands: &Arc<LiveCommands>,
    after: &mut Option<uob_contracts::RequestId>,
) -> io::Result<()> {
    let coordinator =
        CommandCoordinator::new(Arc::new(store.clone()), commands.clone(), Arc::new(Clock));
    let limit = PageLimit::new(100).map_err(|_| unavailable())?;
    let (_, next) = coordinator
        .reconcile_pending_triggers(after.clone(), limit)
        .await
        .map_err(|_| unavailable())?;
    *after = next;
    Ok(())
}
