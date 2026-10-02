use super::charging_profile201_schedule as schedule;
use serde::Deserialize;
use serde_json::Value;
use std::sync::LazyLock;
use uob_contracts::{
    CanonicalResource, ChargingProfile201, ChargingProfileCriteria201, ChargingProfileKind201,
    ChargingProfilePurpose201, ChargingProfileRecurrency201, ClearChargingProfileRequest201,
    CommandErrorCode, NativeProtocolReference, PrivilegedOcppOperation, ProtocolEdition,
    ResourceRef, SetChargingProfileRequest201, UtcTimestamp,
};
mod descriptors;
pub(crate) use descriptors::descriptors;

pub(crate) const ACTIONS: [&str; 2] = ["SetChargingProfile", "ClearChargingProfile"];
pub(crate) const SET_SCHEMA: &str = "urn:OCPP:Cp:2:2020:3:SetChargingProfileRequest";
pub(crate) const CLEAR_SCHEMA: &str = "urn:OCPP:Cp:2:2020:3:ClearChargingProfileRequest";
const SCHEMAS: [&str; 4] = [
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SetChargingProfileRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SetChargingProfileResponse.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ClearChargingProfileRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ClearChargingProfileResponse.json"
    ),
];

pub(crate) fn valid_schema(index: usize, value: &Value) -> bool {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Request {
    Set(SetChargingProfileRequest201),
    Clear(ClearChargingProfileRequest201),
}

pub(crate) fn evse(resource: &ResourceRef) -> Option<i32> {
    match (&resource.resource, resource.native_protocol_reference) {
        (None, None) => Some(0),
        (
            Some(CanonicalResource::Evse {
                connector_id: None, ..
            }),
            Some(NativeProtocolReference::Ocpp201 {
                evse_id,
                connector_id: None,
            }),
        ) if evse_id > 0 => i32::try_from(evse_id).ok(),
        _ => None,
    }
}

pub(super) fn integer(value: &Value) -> Option<i32> {
    i32::try_from(value.as_i64()?).ok()
}
fn optional_integer(value: Option<&Value>) -> Result<Option<i32>, CommandErrorCode> {
    value
        .map(|value| integer(value).ok_or(CommandErrorCode::InvalidParameters))
        .transpose()
}
fn timestamp(value: Option<&Value>) -> Result<Option<UtcTimestamp>, CommandErrorCode> {
    value
        .map(UtcTimestamp::deserialize)
        .transpose()
        .map_err(|_| CommandErrorCode::InvalidParameters)
}
/// Unknown extensions are rejected before native decoding can silently drop them.
fn unsupported(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(name, value)| {
            matches!(name.as_str(), "customData" | "salesTariff") || unsupported(value)
        }),
        Value::Array(values) => values.iter().any(unsupported),
        _ => false,
    }
}

pub(crate) fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<Request, CommandErrorCode> {
    let index = ACTIONS
        .iter()
        .position(|action| *action == operation.action.as_str())
        .ok_or(CommandErrorCode::UnsupportedOperation)?;
    let invalid = CommandErrorCode::InvalidParameters;
    if operation.protocol != ProtocolEdition::Ocpp201
        || operation.payload_schema.as_str() != [SET_SCHEMA, CLEAR_SCHEMA][index]
        || unsupported(&operation.payload)
        || !valid_schema(index * 2, &operation.payload)
    {
        return Err(invalid);
    }
    let scope = evse(resource).ok_or(invalid)?;
    if index == 0 {
        parse_set(scope, &operation.payload)
            .map(Request::Set)
            .ok_or(invalid)
    } else {
        parse_clear(scope, &operation.payload)
            .map(Request::Clear)
            .ok_or(invalid)
    }
}

pub(crate) fn parse_set(scope: i32, payload: &Value) -> Option<SetChargingProfileRequest201> {
    let evse_id = integer(&payload["evseId"])?;
    if scope != evse_id || evse_id < 0 {
        return None;
    }
    let profile = &payload["chargingProfile"];
    let id = integer(&profile["id"])?;
    let stack_level = integer(&profile["stackLevel"])?;
    let purpose =
        ChargingProfilePurpose201::deserialize(&profile["chargingProfilePurpose"]).ok()?;
    let kind = ChargingProfileKind201::deserialize(&profile["chargingProfileKind"]).ok()?;
    let recurrency_kind = profile
        .get("recurrencyKind")
        .map(ChargingProfileRecurrency201::deserialize)
        .transpose()
        .ok()?;
    let transaction_id = profile
        .get("transactionId")
        .map(|v| v.as_str().map(str::to_owned))
        .map_or(Some(None), |value| value.map(Some))?;
    let valid_from = timestamp(profile.get("validFrom")).ok()?;
    let valid_to = timestamp(profile.get("validTo")).ok()?;
    if stack_level < 0
        || (kind == ChargingProfileKind201::Recurring) != recurrency_kind.is_some()
        || (purpose == ChargingProfilePurpose201::TxProfile) != transaction_id.is_some()
        || transaction_id
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.chars().count() > 36)
        || (purpose == ChargingProfilePurpose201::TxProfile && evse_id == 0)
        || (purpose == ChargingProfilePurpose201::ChargingStationMaxProfile
            && (evse_id != 0 || kind == ChargingProfileKind201::Relative))
        || matches!((valid_from, valid_to), (Some(from), Some(to)) if from >= to)
    {
        return None;
    }
    let native = schedule::decode(payload)?;
    let raw = profile["chargingSchedule"].as_array()?;
    if raw.len() != 1 || native.charging_profile.charging_schedule.len() != 1 {
        return None;
    }
    let schedule = schedule::parse(
        &raw[0],
        native
            .charging_profile
            .charging_schedule
            .into_iter()
            .next()?,
    )?;
    if (kind != ChargingProfileKind201::Relative) != schedule.start_schedule.is_some() {
        return None;
    }
    Some(SetChargingProfileRequest201 {
        evse_id,
        charging_profile: ChargingProfile201 {
            id,
            stack_level,
            charging_profile_purpose: purpose,
            charging_profile_kind: kind,
            transaction_id,
            recurrency_kind,
            valid_from,
            valid_to,
            charging_schedule: vec![schedule],
        },
    })
}

fn parse_clear(scope: i32, payload: &Value) -> Option<ClearChargingProfileRequest201> {
    let id = optional_integer(payload.get("chargingProfileId")).ok()?;
    let criteria = if let Some(raw) = payload.get("chargingProfileCriteria") {
        let evse_id = optional_integer(raw.get("evseId")).ok()?;
        let stack_level = optional_integer(raw.get("stackLevel")).ok()?;
        let purpose = raw
            .get("chargingProfilePurpose")
            .map(ChargingProfilePurpose201::deserialize)
            .transpose()
            .ok()?;
        if evse_id.is_some_and(|id| id < 0)
            || stack_level.is_some_and(|level| level < 0)
            || (evse_id.is_none() && stack_level.is_none() && purpose.is_none())
        {
            return None;
        }
        Some(ChargingProfileCriteria201 {
            evse_id,
            stack_level,
            charging_profile_purpose: purpose,
        })
    } else {
        None
    };
    if id.is_some() == criteria.is_some()
        || (scope > 0 && (id.is_some() || criteria.as_ref()?.evse_id != Some(scope)))
    {
        return None;
    }
    rust_ocpp::v2_0_1::messages::clear_charging_profile::ClearChargingProfileRequest::deserialize(
        payload,
    )
    .ok()?;
    Some(ClearChargingProfileRequest201 {
        charging_profile_id: id,
        charging_profile_criteria: criteria,
    })
}
