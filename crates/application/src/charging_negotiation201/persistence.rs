//! Latest-state snapshot points and one typed journal record, committed atomically before the
//! station's CALL is answered. Points are observed state only; they never feed a command.
use super::{evse_resource, limit_point};
use crate::{
    AtomicStoreWrite, ObservationCommitError, OperationalStore, StationEvent, registration,
    transaction16::TransactionContext,
};
use uob_contracts::{
    CHARGING_NEGOTIATION_EVIDENCE_LIMIT_201, ChargingNegotiation201, ChargingScheduleRateUnit201,
    DataPointValue, EvChargingParameters201, EventEnvelope, EventOrigin, EventType,
    NativeProtocolReference, ResourceRef, StationSnapshot, TransactionId, TransactionSnapshot,
    TypedValue, UtcTimestamp,
};

/// Atomically commits one negotiation record, its latest-state points and journal event.
/// The station owner holds its ordered lock and answers the CALL only after this returns.
///
/// # Errors
///
/// Rejects an invalid context, a station without current accepted 2.0.1 registration, an EVSE-
/// scoped limit for an unconfigured EVSE, and failed persistence. The snapshot is unchanged
/// on error.
pub async fn record_charging_negotiation_201<C: Send + 'static, R: Send + 'static>(
    store: &dyn OperationalStore<C, StationEvent, TransactionSnapshot, R>,
    snapshot: &mut StationSnapshot,
    record: ChargingNegotiation201,
    context: TransactionContext,
    now: UtcTimestamp,
) -> Result<(), ObservationCommitError> {
    if context.sequence == 0
        || context.identity.bridge_id != snapshot.station.bridge_id
        || context.identity.selected_target_id != context.target.as_ref().map(|(id, _)| id.clone())
    {
        return Err(ObservationCommitError::InvalidState);
    }
    registration::v201::accepted(snapshot).map_err(|_| ObservationCommitError::InvalidState)?;
    let mut next = snapshot.clone();
    let resource = apply_points(&mut next, &record, now)?;
    registration::activity(&mut next, now);
    let event_type = match &record {
        ChargingNegotiation201::EvChargingNeeds { .. } => "station.ev_charging_needs.201",
        ChargingNegotiation201::EvChargingSchedule { .. } => "station.ev_charging_schedule.201",
        ChargingNegotiation201::ChargingLimit { .. } => "station.charging_limit.201",
        ChargingNegotiation201::ChargingLimitCleared { .. } => "station.charging_limit_cleared.201",
    };
    let event = EventEnvelope {
        event_id: context.event_id,
        schema_version: next.schema_version,
        runtime: context.identity.runtime,
        resource,
        // None of these CALLs carries an observation timestamp.
        source_time: None,
        observed_at: now,
        event_type: EventType::new(event_type).map_err(|_| ObservationCommitError::InvalidState)?,
        origin: EventOrigin::Station,
        sequence: context.sequence,
        correlation_id: context.correlation_id,
        causation_id: None,
        provenance: None,
        payload: StationEvent::ChargingNegotiation201 {
            station_snapshot_invalidated: next.station.station_id.clone(),
            charging_negotiation_201: bounded(record),
        },
    };
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(next.clone());
    write.journal_events.push(event);
    store
        .write_atomic(write)
        .await
        .map_err(ObservationCommitError::Storage)?;
    *snapshot = next;
    Ok(())
}

/// Keeps counts and flags but omits schedules that would exceed the evidence bound.
fn bounded(mut record: ChargingNegotiation201) -> ChargingNegotiation201 {
    let fits = |record: &ChargingNegotiation201| {
        serde_json::to_vec(record)
            .is_ok_and(|encoded| encoded.len() <= CHARGING_NEGOTIATION_EVIDENCE_LIMIT_201)
    };
    if fits(&record) {
        return record;
    }
    match &mut record {
        ChargingNegotiation201::EvChargingSchedule { schedule, .. } => {
            schedule.charging_schedule = None;
        }
        ChargingNegotiation201::ChargingLimit { limit } => {
            let omitted = u32::try_from(limit.charging_schedule.len()).unwrap_or(u32::MAX);
            limit.charging_schedule.clear();
            limit.schedules_omitted = Some(omitted);
        }
        ChargingNegotiation201::EvChargingNeeds { .. }
        | ChargingNegotiation201::ChargingLimitCleared { .. } => {}
    }
    record
}

type Points = Vec<(String, Option<TypedValue>)>;

/// Returns the most specific canonical resource the record concerns.
fn apply_points(
    next: &mut StationSnapshot,
    record: &ChargingNegotiation201,
    now: UtcTimestamp,
) -> Result<ResourceRef, ObservationCommitError> {
    let (evse_id, points, limit) = match record {
        ChargingNegotiation201::EvChargingNeeds { needs, .. } => {
            (Some(needs.evse_id), needs_points(record), false)
        }
        ChargingNegotiation201::EvChargingSchedule { schedule, .. } => {
            (Some(schedule.evse_id), schedule_points(record), false)
        }
        ChargingNegotiation201::ChargingLimit { limit } => {
            let point = |field| limit_point(limit.evse_id, limit.charging_limit_source, field);
            let schedules = u64::try_from(limit.charging_schedule.len()).unwrap_or(u64::MAX)
                + u64::from(limit.schedules_omitted.unwrap_or(0));
            let points = vec![
                (point("active"), Some(TypedValue::Boolean(true))),
                (
                    point("grid_critical"),
                    limit.is_grid_critical.map(TypedValue::Boolean),
                ),
                (
                    point("schedules"),
                    Some(TypedValue::UnsignedInteger(schedules)),
                ),
            ];
            (limit.evse_id, points, true)
        }
        ChargingNegotiation201::ChargingLimitCleared { cleared, .. } => {
            let scope = cleared.evse_id.filter(|id| *id > 0);
            let point = |field| limit_point(scope, cleared.charging_limit_source, field);
            let points = vec![
                (point("active"), Some(TypedValue::Boolean(false))),
                (point("grid_critical"), None),
                (point("schedules"), None),
            ];
            (scope, points, true)
        }
    };
    let Some(evse_id) = evse_id else {
        set_all(&mut next.current_values, points, now);
        return Ok(next.station.clone());
    };
    if evse_resource(next, evse_id).is_none() {
        // Needs and EV schedules for unknown EVSEs are answered and journaled without points;
        // an EVSE-scoped limit must name a configured EVSE.
        return if limit {
            Err(ObservationCommitError::InvalidState)
        } else {
            Ok(next.station.clone())
        };
    }
    let resource = next
        .resources
        .iter_mut()
        .find(|resource| {
            resource.resource.native_protocol_reference
                == Some(NativeProtocolReference::Ocpp201 {
                    evse_id,
                    connector_id: None,
                })
        })
        .ok_or(ObservationCommitError::InvalidState)?;
    set_all(&mut resource.current_values, points, now);
    Ok(resource.resource.clone())
}

/// Every field is written, so values from an earlier AC or DC report never linger.
fn needs_points(record: &ChargingNegotiation201) -> Points {
    let ChargingNegotiation201::EvChargingNeeds {
        needs,
        status,
        reason,
        transaction_id,
    } = record
    else {
        return Vec::new();
    };
    let (ac, dc) = match needs.parameters {
        EvChargingParameters201::Ac(ac) => (Some(ac), None),
        EvChargingParameters201::Dc(dc) => (None, Some(dc)),
    };
    let unsigned =
        |value: Option<u32>| value.map(|value| TypedValue::UnsignedInteger(value.into()));
    let percent = |value: Option<u8>| value.map(|value| TypedValue::UnsignedInteger(value.into()));
    let energy = ac
        .map(|ac| ac.energy_amount)
        .or_else(|| dc.and_then(|dc| dc.energy_amount));
    let current = ac
        .map(|ac| ac.ev_max_current)
        .or(dc.map(|dc| dc.ev_max_current));
    let voltage = ac
        .map(|ac| ac.ev_max_voltage)
        .or(dc.map(|dc| dc.ev_max_voltage));
    named(
        &format!("ocpp201/evse-{}/ev-charging-needs/", needs.evse_id),
        [
            ("status", Some(text(status.as_str()))),
            ("reason", reason.map(|reason| text(reason.as_str()))),
            ("transaction_id", transaction(transaction_id.as_ref())),
            (
                "requested_energy_transfer",
                Some(text(needs.requested_energy_transfer.as_str())),
            ),
            ("departure_time", needs.departure_time.and_then(timestamp)),
            ("max_schedule_tuples", unsigned(needs.max_schedule_tuples)),
            ("energy_amount_wh", unsigned(energy)),
            ("ev_min_current_a", unsigned(ac.map(|ac| ac.ev_min_current))),
            ("ev_max_current_a", unsigned(current)),
            ("ev_max_voltage_v", unsigned(voltage)),
            (
                "ev_max_power_w",
                unsigned(dc.and_then(|dc| dc.ev_max_power)),
            ),
            (
                "state_of_charge_percent",
                percent(dc.and_then(|dc| dc.state_of_charge)),
            ),
            (
                "ev_energy_capacity_wh",
                unsigned(dc.and_then(|dc| dc.ev_energy_capacity)),
            ),
            ("full_soc_percent", percent(dc.and_then(|dc| dc.full_soc))),
            ("bulk_soc_percent", percent(dc.and_then(|dc| dc.bulk_soc))),
        ],
    )
}

/// A summary of the decision; the exact schedule is retained only in the journal record.
fn schedule_points(record: &ChargingNegotiation201) -> Points {
    let ChargingNegotiation201::EvChargingSchedule {
        schedule,
        status,
        basis,
        reason,
        transaction_id,
    } = record
    else {
        return Vec::new();
    };
    let native = schedule.charging_schedule.as_ref();
    named(
        &format!("ocpp201/evse-{}/ev-charging-schedule/", schedule.evse_id),
        [
            ("status", Some(text(status.as_str()))),
            ("basis", Some(text(basis.as_str()))),
            ("reason", reason.map(|reason| text(reason.as_str()))),
            ("transaction_id", transaction(transaction_id.as_ref())),
            ("time_base", timestamp(schedule.time_base)),
            (
                "schedule_id",
                native.map(|native| TypedValue::SignedInteger(native.id.into())),
            ),
            (
                "charging_rate_unit",
                native.map(|native| {
                    text(match native.charging_rate_unit {
                        ChargingScheduleRateUnit201::A => "A",
                        ChargingScheduleRateUnit201::W => "W",
                    })
                }),
            ),
            (
                "duration_seconds",
                native
                    .and_then(|native| native.duration)
                    .map(|seconds| TypedValue::SignedInteger(seconds.into())),
            ),
            (
                "period_count",
                Some(TypedValue::UnsignedInteger(schedule.period_count.into())),
            ),
        ],
    )
}

fn named<const N: usize>(prefix: &str, fields: [(&str, Option<TypedValue>); N]) -> Points {
    fields
        .into_iter()
        .map(|(name, value)| (format!("{prefix}{name}"), value))
        .collect()
}

fn set_all(values: &mut Vec<DataPointValue>, points: Points, now: UtcTimestamp) {
    for (id, value) in points {
        registration::set(values, &id, value, None, now);
    }
}

fn text(value: &str) -> TypedValue {
    TypedValue::Text(value.to_owned())
}

fn transaction(value: Option<&TransactionId>) -> Option<TypedValue> {
    value.map(|id| text(id.as_str()))
}

fn timestamp(value: UtcTimestamp) -> Option<TypedValue> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(text))
}
