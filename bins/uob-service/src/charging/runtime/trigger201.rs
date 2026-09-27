//! Native OCPP 2.0.1 trigger evidence, independent of the command response.
use std::io;

use uob_application::OperationalStore;
use uob_contracts::{
    ContractVersion, EventEnvelope, EventOrigin, EventType, ResourceRef, ServiceIdentity,
    StationEvent, TriggerMessageClass201, TriggerTarget201, UtcTimestamp,
};

use super::{event_identity, unavailable};
use crate::charging::ChargingStore;

pub(super) async fn marker(
    store: &ChargingStore,
    station: &ResourceRef,
    identity: &ServiceIdentity,
    class: TriggerMessageClass201,
    target: TriggerTarget201,
    status: Option<&str>,
    now: UtcTimestamp,
) -> io::Result<Option<EventEnvelope<StationEvent>>> {
    if !store
        .trigger_pending_for_station_201(station.clone(), class, now)
        .await
        .map_err(|_| unavailable())?
    {
        return Ok(None);
    }

    let (sequence, event_id) = event_identity(store, identity).await?;
    Ok(Some(EventEnvelope {
        event_id,
        schema_version: ContractVersion::V1_INITIAL,
        runtime: identity.runtime.clone(),
        resource: station.clone(),
        source_time: None,
        observed_at: now,
        event_type: EventType::new("station.trigger.notification.201")
            .map_err(|_| unavailable())?,
        origin: EventOrigin::Station,
        sequence,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StationEvent::TriggerNotification201 {
            station_snapshot_invalidated: station.station_id.clone(),
            trigger_class_201: class,
            target,
            status: status.map(str::to_owned),
        },
    }))
}
