//! OCPP 1.6 availability observations and conservative durable effect reconciliation.
use crate::{DecodedCall, OcppCallError, OcppErrorCode};
use serde_json::{Value, json};
use uob_application::{
    OperationalStore,
    registration::availability::{self, AvailabilityContext},
};
use uob_contracts::{
    Command, CommandOperation, DataPointValue, EventEnvelope, EventOrigin, NativeProtocolReference,
    ObservedCommandEffect, ProtocolEdition, StationSnapshot, TransactionState, TypedValue,
    UtcTimestamp,
};

/// Handles `StatusNotification` using an atomic snapshot/journal commit before returning a reply.
/// Call from the authenticated station's ordered owner, then publish the committed snapshot to
/// its remote-control session. Other registration actions use `complete_registration`.
///
/// # Errors
/// Rejects other actions, invalid status/topology and failed persistence with a sanitized CALLERROR.
pub async fn complete_status<C, E, D, R>(
    call: DecodedCall,
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    context: AvailabilityContext,
    now: UtcTimestamp,
) -> Result<Value, OcppCallError>
where
    C: Send + 'static,
    E: From<StationSnapshot> + Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    let uob_application::ChargerObservation::ConnectorStatus(observation) = call.observation else {
        return Err(OcppCallError {
            protocol: ProtocolEdition::Ocpp16j,
            code: OcppErrorCode::NotImplemented,
            description: "Availability requires a status notification",
            field_path: None,
        });
    };
    availability::record_status(store, snapshot, &observation, context, now)
        .await
        .map_err(|error| super::registration::lifecycle_error(&error))?;
    Ok(json!([3, call.message_id, {}]))
}

/// Links fresh committed station evidence to a requested availability, never to another command.
/// Read `event` from the authoritative journal before calling this, then use the ordinary
/// coordinator's `reconcile_observed_effect`. This is compatible state, not unique causation:
/// OCPP 1.6 `StatusNotification` has no remote-request identifier. The native command response
/// remains separate, including Scheduled or uncertain transmission, through reconciliation.
/// Station-wide requests require fresh evidence for the controller AND every known connector.
#[must_use]
pub fn observed_effect(
    command: &Command<Value>,
    event: &EventEnvelope<StationSnapshot>,
) -> Option<ObservedCommandEffect> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return None;
    };
    let snapshot = &event.payload;
    if operation.protocol != ProtocolEdition::Ocpp16j
        || operation.action.as_str() != "ChangeAvailability"
        || operation.payload_schema.as_str() != "urn:OCPP:1.6:2019:12:ChangeAvailabilityRequest"
        || event.origin != EventOrigin::Station
        || event.provenance.is_some()
        || event.event_type.as_str() != "station.availability.observed"
        || event.resource != snapshot.station
        || event.resource.bridge_id != command.resource.bridge_id
        || event.resource.station_id != command.resource.station_id
        || event.observed_at < command.admitted_at
        || snapshot.observed_at != event.observed_at
    {
        return None;
    }
    let payload = operation.payload.as_object()?;
    if payload.len() != 2 {
        return None;
    }
    let native = payload.get("connectorId")?.as_u64()?;
    let native = u32::try_from(native).ok()?;
    if command.resource.native_protocol_reference
        != Some(NativeProtocolReference::Ocpp16 {
            connector_id: native,
        })
        || (native == 0) != (command.resource == snapshot.station)
    {
        return None;
    }
    let operative = match payload.get("type")?.as_str()? {
        "Operative" => true,
        "Inoperative" => false,
        _ => return None,
    };
    if native == 0 {
        if snapshot.resources.is_empty()
            || !matches_status(&snapshot.current_values, 0, operative, command.admitted_at)
            || !snapshot.resources.iter().all(|entry| {
                let Some(NativeProtocolReference::Ocpp16 { connector_id }) =
                    entry.resource.native_protocol_reference
                else {
                    return false;
                };
                connector_id != 0
                    && matches_status(
                        &entry.current_values,
                        connector_id,
                        operative,
                        command.admitted_at,
                    )
            })
        {
            return None;
        }
    } else {
        let entry = snapshot
            .resources
            .iter()
            .find(|entry| entry.resource == command.resource)?;
        if !matches_status(
            &entry.current_values,
            native,
            operative,
            command.admitted_at,
        ) {
            return None;
        }
    }
    // A status does not end an active transaction. Inoperative completion waits for both facts.
    if !operative
        && snapshot.transactions.iter().any(|tx| {
            tx.state != TransactionState::Ended && (native == 0 || tx.resource == command.resource)
        })
    {
        return None;
    }
    Some(ObservedCommandEffect {
        event_id: event.event_id.clone(),
        event_type: event.event_type.clone(),
        observed_at: event.observed_at,
    })
}

fn matches_status(
    values: &[DataPointValue],
    connector: u32,
    operative: bool,
    admitted: UtcTimestamp,
) -> bool {
    let id = format!("ocpp16/connector-{connector}/status/status");
    values.iter().any(|value| {
        value.point_id.as_str() == id
            && value.observed_at >= admitted
            && value.source_time.unwrap_or(value.observed_at) >= admitted
            && value.quality.level == uob_contracts::QualityLevel::Good
            && matches!(&value.value, Some(TypedValue::Text(status)) if
                if operative { matches!(status.as_str(), "Available" | "Preparing" | "Charging" | "SuspendedEVSE" | "SuspendedEV" | "Finishing" | "Reserved") }
                else { status == "Unavailable" })
    })
}
