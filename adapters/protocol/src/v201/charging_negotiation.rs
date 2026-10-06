//! Charger-initiated OCPP 2.0.1 smart-charging negotiation (K11-K17): pinned-schema and
//! native-semantic decoding, native answers, and the bridge's own installed `TxProfile`.
use super::remote_control::schedule_values;
use crate::{DecodeError, DecodeErrorKind, command_registry::charging_profile201};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::LazyLock;
use uob_application::{ChargerObservation, CsmsTxProfile201, NegotiationObservation201};
use uob_contracts::{
    AcChargingParameters201, ChargingLimitSource201, ChargingNegotiation201,
    ChargingProfileKind201, ChargingProfilePurpose201, ChargingSchedule201,
    ChargingSchedulePeriod201, ChargingScheduleRateUnit201, ClearedChargingLimit201, Command,
    CommandOperation, DcChargingParameters201, EnergyTransferMode201, EvChargingNeeds201,
    EvChargingParameters201, EvChargingSchedule201, ExactDecimal, ExternalChargingLimit201,
    NegotiationReason201, ProtocolEdition, UtcTimestamp,
};

/// Charger-originated actions this module decodes; responses share the index order.
const ACTIONS: [&str; 4] = [
    "NotifyEVChargingNeeds",
    "NotifyEVChargingSchedule",
    "NotifyChargingLimit",
    "ClearedChargingLimit",
];
const SCHEMAS: [&str; 8] = [
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/NotifyEVChargingNeedsRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/NotifyEVChargingScheduleRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/NotifyChargingLimitRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ClearedChargingLimitRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/NotifyEVChargingNeedsResponse.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/NotifyEVChargingScheduleResponse.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/NotifyChargingLimitResponse.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ClearedChargingLimitResponse.json"
    ),
];
const RESPONSE: usize = 4;

fn valid_schema(index: usize, value: &Value) -> bool {
    static VALIDATORS: LazyLock<Vec<jsonschema::Validator>> = LazyLock::new(|| {
        SCHEMAS
            .iter()
            .map(|source| {
                let schema: Value = serde_json::from_str(source).expect("pinned native schema");
                jsonschema::options()
                    .should_validate_formats(true)
                    .build(&schema)
                    .expect("pinned native schema")
            })
            .collect()
    });
    VALIDATORS[index].is_valid(value)
}

fn invalid() -> DecodeError {
    DecodeError::new(ProtocolEdition::Ocpp201, DecodeErrorKind::InvalidPayload)
}

/// Decodes one negotiation CALL. The pinned schema is a floor: native semantics additionally
/// require positive EVSEs where the specification does, exactly one parameter set matching
/// the transfer mode, ordered exact-tenths schedules and a non-CSO external limit.
/// `customData` is never retained.
pub(crate) fn observation(
    action: &str,
    payload: &Value,
) -> Result<ChargerObservation, DecodeError> {
    let index = ACTIONS
        .iter()
        .position(|candidate| *candidate == action)
        .ok_or_else(invalid)?;
    if !valid_schema(index, payload) {
        return Err(invalid());
    }
    let observation = match index {
        0 => NegotiationObservation201::EvChargingNeeds(needs(payload).ok_or_else(invalid)?),
        1 => {
            NegotiationObservation201::EvChargingSchedule(ev_schedule(payload).ok_or_else(invalid)?)
        }
        2 => NegotiationObservation201::ChargingLimit(limit(payload).ok_or_else(invalid)?),
        _ => NegotiationObservation201::ChargingLimitCleared(cleared(payload).ok_or_else(invalid)?),
    };
    Ok(ChargerObservation::ChargingNegotiation201(observation))
}

/// The exact native response payload for a committed record.
#[must_use]
pub fn response(record: &ChargingNegotiation201) -> Value {
    let (index, status, reason) = match record {
        ChargingNegotiation201::EvChargingNeeds { status, reason, .. } => {
            (0, Some(status.as_str()), *reason)
        }
        ChargingNegotiation201::EvChargingSchedule { status, reason, .. } => {
            (1, Some(status.as_str()), *reason)
        }
        ChargingNegotiation201::ChargingLimit { .. } => (2, None, None),
        ChargingNegotiation201::ChargingLimitCleared { .. } => (3, None, None),
    };
    let mut payload = status.map_or_else(|| json!({}), |status| json!({ "status": status }));
    if let Some(reason) = reason.map(NegotiationReason201::as_str) {
        payload["statusInfo"] = json!({ "reasonCode": reason });
    }
    debug_assert!(valid_schema(RESPONSE + index, &payload));
    payload
}

/// The bridge's own accepted `TxProfile`, rebuilt from its retained command: a canonical
/// charging limit is one unbounded relative period at stack zero. Returns `None` for any
/// other command so the caller treats the profile as unverifiable.
#[must_use]
pub fn csms_tx_profile(command: &Command<Value>, profile_id: i32) -> Option<CsmsTxProfile201> {
    match &command.operation {
        CommandOperation::SetChargingLimit(limit) => {
            let (unit, quantity) = crate::remote_constraints::profile_quantity(limit).ok()?;
            Some(CsmsTxProfile201 {
                stack_level: 0,
                kind: ChargingProfileKind201::Relative,
                valid_from: None,
                valid_to: None,
                schedule: ChargingSchedule201 {
                    id: profile_id,
                    duration: None,
                    start_schedule: None,
                    charging_rate_unit: if unit == "W" {
                        ChargingScheduleRateUnit201::W
                    } else {
                        ChargingScheduleRateUnit201::A
                    },
                    charging_schedule_period: vec![ChargingSchedulePeriod201 {
                        start_period: 0,
                        limit: quantity.to_string().parse::<ExactDecimal>().ok()?,
                        number_phases: limit.phases.map(i32::from),
                        phase_to_use: None,
                    }],
                    min_charging_rate: None,
                },
            })
        }
        CommandOperation::Ocpp(operation)
            if operation.protocol == ProtocolEdition::Ocpp201
                && operation.action.as_str() == "SetChargingProfile" =>
        {
            let scope = charging_profile201::evse(&command.resource)?;
            let profile =
                charging_profile201::parse_set(scope, &operation.payload)?.charging_profile;
            if profile.id != profile_id
                || profile.charging_profile_purpose != ChargingProfilePurpose201::TxProfile
            {
                return None;
            }
            let [schedule] =
                <[ChargingSchedule201; 1]>::try_from(profile.charging_schedule).ok()?;
            Some(CsmsTxProfile201 {
                stack_level: profile.stack_level,
                kind: profile.charging_profile_kind,
                valid_from: profile.valid_from,
                valid_to: profile.valid_to,
                schedule,
            })
        }
        _ => None,
    }
}

fn positive(value: &Value) -> Option<u32> {
    u32::try_from(value.as_i64()?)
        .ok()
        .filter(|value| *value > 0)
}

fn nonnegative(value: &Value) -> Option<u32> {
    u32::try_from(value.as_i64()?)
        .ok()
        .filter(|value| i32::try_from(*value).is_ok())
}

/// A supplied optional field that is not a valid value.
struct Invalid;

/// A supplied optional field must be valid; an absent one stays absent.
fn optional<T>(
    value: Option<&Value>,
    parse: impl Fn(&Value) -> Option<T>,
) -> Result<Option<T>, Invalid> {
    value.map_or(Ok(None), |value| parse(value).map(Some).ok_or(Invalid))
}

fn percent(value: &Value) -> Option<u8> {
    u8::try_from(value.as_u64()?)
        .ok()
        .filter(|value| *value <= 100)
}

fn timestamp(value: &Value) -> Option<UtcTimestamp> {
    UtcTimestamp::deserialize(value).ok()
}

fn needs(payload: &Value) -> Option<EvChargingNeeds201> {
    let needs = &payload["chargingNeeds"];
    let mode = EnergyTransferMode201::deserialize(&needs["requestedEnergyTransfer"]).ok()?;
    let parameters = match (
        mode,
        needs.get("acChargingParameters"),
        needs.get("dcChargingParameters"),
    ) {
        (EnergyTransferMode201::Dc, None, Some(dc)) => {
            EvChargingParameters201::Dc(DcChargingParameters201 {
                ev_max_current: nonnegative(&dc["evMaxCurrent"])?,
                ev_max_voltage: nonnegative(&dc["evMaxVoltage"])?,
                energy_amount: optional(dc.get("energyAmount"), nonnegative).ok()?,
                ev_max_power: optional(dc.get("evMaxPower"), nonnegative).ok()?,
                state_of_charge: optional(dc.get("stateOfCharge"), percent).ok()?,
                ev_energy_capacity: optional(dc.get("evEnergyCapacity"), nonnegative).ok()?,
                full_soc: optional(dc.get("fullSoC"), percent).ok()?,
                bulk_soc: optional(dc.get("bulkSoC"), percent).ok()?,
            })
        }
        (
            EnergyTransferMode201::AcSinglePhase
            | EnergyTransferMode201::AcTwoPhase
            | EnergyTransferMode201::AcThreePhase,
            Some(ac),
            None,
        ) => {
            let ac = AcChargingParameters201 {
                energy_amount: nonnegative(&ac["energyAmount"])?,
                ev_min_current: nonnegative(&ac["evMinCurrent"])?,
                ev_max_current: nonnegative(&ac["evMaxCurrent"])?,
                ev_max_voltage: nonnegative(&ac["evMaxVoltage"])?,
            };
            if ac.ev_min_current > ac.ev_max_current {
                return None;
            }
            EvChargingParameters201::Ac(ac)
        }
        // K15.FR.06 / K17.FR.06: exactly one parameter set, consistent with the mode.
        _ => return None,
    };
    Some(EvChargingNeeds201 {
        evse_id: positive(&payload["evseId"])?,
        max_schedule_tuples: optional(payload.get("maxScheduleTuples"), positive).ok()?,
        requested_energy_transfer: mode,
        departure_time: optional(needs.get("departureTime"), timestamp).ok()?,
        parameters,
    })
}

fn ev_schedule(payload: &Value) -> Option<EvChargingSchedule201> {
    let (schedule, sales_tariff_omitted) = schedule_values::schedule(&payload["chargingSchedule"])?;
    Some(EvChargingSchedule201 {
        evse_id: positive(&payload["evseId"])?,
        time_base: timestamp(&payload["timeBase"])?,
        period_count: u32::try_from(schedule.charging_schedule_period.len()).ok()?,
        charging_schedule: Some(schedule),
        sales_tariff_omitted,
    })
}

fn limit(payload: &Value) -> Option<ExternalChargingLimit201> {
    let source =
        ChargingLimitSource201::deserialize(&payload["chargingLimit"]["chargingLimitSource"])
            .ok()?;
    // K11.FR.05 / K12.FR.04: CSO limits are installed by this CSMS, never reported back.
    if source == ChargingLimitSource201::Cso {
        return None;
    }
    let mut sales_tariff_omitted = false;
    let mut charging_schedule = Vec::new();
    for raw in payload
        .get("chargingSchedule")
        .map_or(Some(&[][..]), |raw| raw.as_array().map(Vec::as_slice))?
    {
        let (schedule, tariff) = schedule_values::schedule(raw)?;
        sales_tariff_omitted |= tariff;
        charging_schedule.push(schedule);
    }
    Some(ExternalChargingLimit201 {
        evse_id: optional(payload.get("evseId"), positive).ok()?,
        charging_limit_source: source,
        is_grid_critical: optional(
            payload["chargingLimit"].get("isGridCritical"),
            Value::as_bool,
        )
        .ok()?,
        charging_schedule,
        schedules_omitted: None,
        sales_tariff_omitted,
    })
}

fn cleared(payload: &Value) -> Option<ClearedChargingLimit201> {
    Some(ClearedChargingLimit201 {
        charging_limit_source: ChargingLimitSource201::deserialize(&payload["chargingLimitSource"])
            .ok()?,
        evse_id: optional(payload.get("evseId"), nonnegative).ok()?,
    })
}
