//! Scoped OCPP 2.0.1 availability commands and durable connector evidence.
use crate::{DecodedCall, OcppCallError, OcppErrorCode};
use rust_ocpp::v2_0_1::messages::change_availability::ChangeAvailabilityRequest;
use serde_json::{Value, json};
use uob_application::{
    OperationalStore,
    registration::availability::{self, AvailabilityContext},
};
use uob_contracts::{
    Command, CommandErrorCode, CommandOperation, EventEnvelope, EventOrigin,
    NativeProtocolReference, ObservedCommandEffect, PrivilegedOcppOperation, ProtocolEdition,
    QualityLevel, ResourceRef, StationSnapshot, TransactionState, TypedValue, UtcTimestamp,
};

pub(super) fn prepare(
    operation: &PrivilegedOcppOperation<Value>,
    resource: &ResourceRef,
    station: &ResourceRef,
) -> Result<(&'static str, Value), CommandErrorCode> {
    let invalid = CommandErrorCode::InvalidParameters;
    if operation.protocol != ProtocolEdition::Ocpp201
        || operation.payload_schema.as_str() != "urn:OCPP:Cp:2:2020:3:ChangeAvailabilityRequest"
    {
        return Err(invalid);
    }
    let payload = operation.payload.as_object().ok_or(invalid)?;
    if payload
        .keys()
        .any(|k| !matches!(k.as_str(), "evse" | "operationalStatus"))
    {
        return Err(invalid);
    }
    let request: ChangeAvailabilityRequest =
        serde_json::from_value(operation.payload.clone()).map_err(|_| invalid)?;
    let expected = if resource == station {
        None
    } else {
        match resource.native_protocol_reference {
            Some(NativeProtocolReference::Ocpp201 {
                evse_id,
                connector_id,
            }) if evse_id > 0
                && i32::try_from(evse_id).is_ok()
                && connector_id.is_none_or(|id| id > 0 && i32::try_from(id).is_ok()) =>
            {
                let mut evse = json!({"id":evse_id});
                if let Some(id) = connector_id {
                    evse["connectorId"] = json!(id);
                }
                Some(evse)
            }
            _ => return Err(invalid),
        }
    };
    // Exact nested shape rejects unknown fields and nulls instead of silently widening scope.
    if payload.get("evse") != expected.as_ref() {
        return Err(invalid);
    }
    Ok((
        "ChangeAvailability",
        serde_json::to_value(request).map_err(|_| invalid)?,
    ))
}

/// Commits `StatusNotification` and its journal evidence before acknowledging the charger.
/// The authenticated station owner serializes this with transaction and registration updates.
/// # Errors
/// Rejects invalid topology, registration state, and failed persistence with sanitized CALLERROR.
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
    let uob_application::ChargerObservation::EvseConnectorStatus(observation) = call.observation
    else {
        return Err(OcppCallError {
            protocol: ProtocolEdition::Ocpp201,
            code: OcppErrorCode::NotImplemented,
            description: "Availability requires a status notification",
            field_path: None,
        });
    };
    availability::record_status_201(store, snapshot, &observation, context, now)
        .await
        .map_err(|e| super::registration::lifecycle_error(&e))?;
    Ok(json!([3, call.message_id, {}]))
}

/// Links compatible fresh committed status, never inferred command causation.
/// Read the event from the authoritative journal, then use ordinary coordinator reconciliation.
/// Station/EVSE requests require every known connector in scope. Occupied and Reserved are
/// operative evidence, not proof of charging. Inoperative also requires transaction completion.
#[must_use]
pub fn observed_effect(
    command: &Command<Value>,
    event: &EventEnvelope<StationSnapshot>,
) -> Option<ObservedCommandEffect> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return None;
    };
    let snapshot = &event.payload;
    if operation.action.as_str() != "ChangeAvailability"
        || prepare(operation, &command.resource, &snapshot.station).is_err()
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
    let operative = operation.payload["operationalStatus"] == "Operative";
    let mut count = 0;
    for entry in &snapshot.resources {
        let Some(NativeProtocolReference::Ocpp201 {
            evse_id,
            connector_id: Some(connector_id),
        }) = entry.resource.native_protocol_reference
        else {
            continue;
        };
        if !covers(&command.resource, &entry.resource) {
            continue;
        }
        count += 1;
        let id = format!("ocpp201/evse-{evse_id}/connector-{connector_id}/status/status");
        if !entry.current_values.iter().any(|v| {
            v.point_id.as_str() == id
                && v.observed_at >= command.admitted_at
                && v.source_time.is_some_and(|t| t >= command.admitted_at)
                && v.quality.level == QualityLevel::Good
                && matches!(&v.value, Some(TypedValue::Text(status)) if
                if operative { matches!(status.as_str(), "Available" | "Occupied" | "Reserved") }
                else { status == "Unavailable" })
        }) {
            return None;
        }
    }
    if count == 0
        || (!operative
            && snapshot.transactions.iter().any(|tx| {
                tx.state != TransactionState::Ended && overlaps(&command.resource, &tx.resource)
            }))
    {
        return None;
    }
    Some(ObservedCommandEffect {
        event_id: event.event_id.clone(),
        event_type: event.event_type.clone(),
        observed_at: event.observed_at,
    })
}

fn covers(scope: &ResourceRef, resource: &ResourceRef) -> bool {
    scope.bridge_id == resource.bridge_id
        && scope.station_id == resource.station_id
        && (scope.resource.is_none()
            || scope == resource
            || matches!(
            (scope.native_protocol_reference, resource.native_protocol_reference),
            (Some(NativeProtocolReference::Ocpp201 { evse_id:a, connector_id:None }),
             Some(NativeProtocolReference::Ocpp201 { evse_id:b, .. })) if a == b && a > 0))
}
fn overlaps(scope: &ResourceRef, resource: &ResourceRef) -> bool {
    covers(scope, resource) || covers(resource, scope)
}
