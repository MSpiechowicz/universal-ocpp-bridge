//! Persisted station topology, reconnect snapshots, and scoped invalidation events.
use std::{collections::BTreeMap, io};

use uob_application::{AtomicStoreWrite, OperationalStore};
use uob_contracts::{
    AvailabilityState, ChargingResourceSnapshot, Connectivity, ContractVersion, EventEnvelope,
    EventOrigin, EventType, ProtocolEdition, ResourceCapabilities, ResourceRef, ServiceIdentity,
    StationEvent, StationId, StationSnapshot, UtcTimestamp,
};

use super::{Clock, event_identity, unavailable};
use crate::charging::{ChargingStore, StationSettings};

pub(crate) async fn reconcile(
    store: &ChargingStore,
    resources: &BTreeMap<StationId, Vec<ResourceRef>>,
    settings: &BTreeMap<StationId, StationSettings>,
    identity: &ServiceIdentity,
) -> io::Result<()> {
    for expected in resources.values() {
        let Some(mut snapshot) = store
            .station_snapshot(expected[0].clone())
            .await
            .map_err(|_| unavailable())?
        else {
            continue;
        };
        if snapshot.station != expected[0]
            || snapshot
                .resources
                .iter()
                .map(|item| &item.resource)
                .ne(expected.iter().skip(1))
        {
            return Err(io::Error::other(
                "charging topology differs from persisted station state",
            ));
        }
        let configuration = settings
            .get(&expected[0].station_id)
            .ok_or_else(unavailable)?;
        let previous = (
            snapshot.capabilities.clone(),
            snapshot
                .resources
                .iter()
                .map(|entry| entry.capabilities.clone())
                .collect::<Vec<_>>(),
        );
        configuration.apply_capabilities(&mut snapshot);
        let capabilities_changed = previous.0 != snapshot.capabilities
            || previous.1
                != snapshot
                    .resources
                    .iter()
                    .map(|entry| entry.capabilities.clone())
                    .collect::<Vec<_>>();
        if snapshot.connectivity != Connectivity::Disconnected || capabilities_changed {
            snapshot.connectivity = Connectivity::Disconnected;
            for resource in &mut snapshot.resources {
                resource.availability = AvailabilityState::Unknown;
            }
            store_snapshot(store, snapshot, identity).await?;
        }
    }
    Ok(())
}

pub(super) async fn connected_snapshot(
    store: &ChargingStore,
    resources: &[ResourceRef],
    configuration: &StationSettings,
    protocol: ProtocolEdition,
    identity: &ServiceIdentity,
) -> io::Result<StationSnapshot> {
    use uob_application::CommandClock;

    let now = Clock.now();
    let mut snapshot = store
        .station_snapshot(resources[0].clone())
        .await
        .map_err(|_| unavailable())?
        .unwrap_or_else(|| StationSnapshot {
            schema_version: ContractVersion::V1_INITIAL,
            station: resources[0].clone(),
            observed_at: now,
            connectivity: Connectivity::Disconnected,
            capabilities: ResourceCapabilities::default(),
            resources: resources
                .iter()
                .skip(1)
                .cloned()
                .map(|resource| ChargingResourceSnapshot {
                    resource,
                    availability: AvailabilityState::Unknown,
                    capabilities: ResourceCapabilities::default(),
                    data_points: vec![],
                    current_values: vec![],
                })
                .collect(),
            transactions: vec![],
            current_values: vec![],
        });
    configuration.apply_capabilities(&mut snapshot);
    snapshot.connectivity = Connectivity::Connected {
        protocol,
        connected_at: now,
        last_message_at: None,
    };
    // A transport reconnect is not a new accepted BootNotification, even after a clean restart.
    snapshot
        .current_values
        .retain(|value| !value.point_id.as_str().ends_with("/registration/status"));
    snapshot.observed_at = now;
    store_snapshot(store, snapshot.clone(), identity).await?;
    Ok(snapshot)
}

pub(super) async fn store_snapshot(
    store: &ChargingStore,
    snapshot: StationSnapshot,
    identity: &ServiceIdentity,
) -> io::Result<()> {
    let event = invalidation(store, &snapshot, identity, snapshot.observed_at).await?;
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(snapshot);
    write.journal_events.push(event);
    store.write_atomic(write).await.map_err(|_| unavailable())?;
    Ok(())
}

pub(super) async fn invalidation(
    store: &ChargingStore,
    snapshot: &StationSnapshot,
    identity: &ServiceIdentity,
    observed_at: UtcTimestamp,
) -> io::Result<EventEnvelope<StationEvent>> {
    let (sequence, event_id) = event_identity(store, identity).await?;
    Ok(EventEnvelope {
        event_id,
        schema_version: snapshot.schema_version,
        runtime: identity.runtime.clone(),
        resource: snapshot.station.clone(),
        source_time: None,
        observed_at,
        event_type: EventType::new("station.snapshot.invalidated").map_err(|_| unavailable())?,
        origin: EventOrigin::Bridge,
        sequence,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StationEvent::Invalidation {
            station_snapshot_invalidated: snapshot.station.station_id.clone(),
        },
    })
}
