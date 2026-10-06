use super::{ReservationObservation16, ReservationObservationKind16};
use crate::registration::ConnectorStatusObservation;
use uob_contracts::{
    NativeProtocolReference, QualityLevel, ReservationState16, StationSnapshot, TypedValue,
    UtcTimestamp,
};

pub(crate) fn observations(
    snapshot: &StationSnapshot,
    observation: &ConnectorStatusObservation,
    now: UtcTimestamp,
) -> Vec<ReservationObservation16> {
    let mut result = vec![ReservationObservation16 {
        station: snapshot.station.clone(),
        observed_at: now,
        kind: ReservationObservationKind16::Expiry,
    }];
    let state = match observation.status.as_str() {
        "Faulted" => ReservationState16::Faulted,
        "Unavailable" => ReservationState16::Unavailable,
        _ => return result,
    };
    let status_id = format!(
        "ocpp16/connector-{}/status/status",
        observation.connector_id
    );
    let fresh = snapshot
        .current_values
        .iter()
        .chain(snapshot.resources.iter().flat_map(|r| &r.current_values))
        .any(|v| {
            v.point_id.as_str() == status_id
                && v.observed_at == now
                && v.source_time == observation.source_time
                && matches!(&v.value, Some(TypedValue::Text(s)) if s == &observation.status)
        });
    if !fresh {
        return result;
    }
    // Unknown/stale other connectors remain potentially eligible. Never bind Any to one child.
    let any_eligible = snapshot.resources.iter().any(|entry| {
        let Some(NativeProtocolReference::Ocpp16 { connector_id }) = entry.resource.native_protocol_reference else { return true; };
        if connector_id == 0 { return false; }
        !entry.current_values.iter().any(|value| {
            value.point_id.as_str() == format!("ocpp16/connector-{connector_id}/status/status")
                && value.quality.level == QualityLevel::Good
                && matches!(&value.value, Some(TypedValue::Text(status)) if matches!(status.as_str(), "Faulted" | "Unavailable"))
        })
    });
    result.push(ReservationObservation16 {
        station: snapshot.station.clone(),
        observed_at: now,
        kind: ReservationObservationKind16::Status {
            connector_id: observation.connector_id,
            state,
            source_time: observation.source_time,
            any_eligible,
        },
    });
    result
}
