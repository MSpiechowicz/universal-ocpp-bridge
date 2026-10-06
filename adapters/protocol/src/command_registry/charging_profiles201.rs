//! Pinned OCPP 2.0.1 `GetChargingProfiles` (K09) request scope, criteria and schemas.
use super::{CommandSchemaDescriptor, CommandSchemaField, charging_profile201::evse};
use rust_ocpp::v2_0_1::messages::get_charging_profiles::GetChargingProfilesRequest;
use serde::Deserialize;
use serde_json::Value;
use std::sync::LazyLock;
use uob_contracts::{
    ChargingLimitSource201, ChargingProfilesQuery201, CommandErrorCode, Operation,
    PrivilegedOcppOperation, ProtocolEdition, ReportedChargingProfilePurpose201, ResourceRef,
    StationSnapshot, ValueType,
};

pub(crate) const ACTION: &str = "GetChargingProfiles";
pub(crate) const REPORT_ACTION: &str = "ReportChargingProfiles";
pub(crate) const SCHEMA: &str = "urn:OCPP:Cp:2:2020:3:GetChargingProfilesRequest";
pub(crate) const REQUEST: usize = 0;
pub(crate) const RESPONSE: usize = 1;
pub(crate) const REPORT: usize = 2;
/// Bounded explicit ID filter; the native `ChargingProfileEntries` limit is never assumed larger.
pub(crate) const MAX_PROFILE_IDS: usize = 64;
const SCHEMAS: [&str; 4] = [
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetChargingProfilesRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetChargingProfilesResponse.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ReportChargingProfilesRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ReportChargingProfilesResponse.json"
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

fn field(name: &'static str, value_type: ValueType, required: bool) -> CommandSchemaField {
    let enum_values = if name.ends_with("chargingProfilePurpose") {
        Some(vec![
            "ChargingStationExternalConstraints",
            "ChargingStationMaxProfile",
            "TxDefaultProfile",
            "TxProfile",
        ])
    } else if name.ends_with("chargingLimitSource[]") {
        Some(vec!["EMS", "Other", "SO", "CSO"])
    } else {
        None
    };
    CommandSchemaField {
        name,
        value_type,
        required,
        enum_values,
    }
}

/// Station scope and each advertised positive EVSE, never a connector.
pub(crate) fn descriptors(snapshot: &StationSnapshot) -> Vec<CommandSchemaDescriptor> {
    let required = Operation::ProtocolAction {
        protocol: ProtocolEdition::Ocpp201,
        action: ACTION.to_owned(),
    };
    std::iter::once((&snapshot.station, &snapshot.capabilities))
        .chain(
            snapshot
                .resources
                .iter()
                .map(|entry| (&entry.resource, &entry.capabilities)),
        )
        .filter(|(resource, capabilities)| {
            resource.bridge_id == snapshot.station.bridge_id
                && resource.station_id == snapshot.station.station_id
                && evse(resource).is_some()
                && capabilities.supports(&required)
        })
        .map(|(resource, _)| CommandSchemaDescriptor {
            resource: resource.clone(),
            protocol: ProtocolEdition::Ocpp201,
            action: ACTION,
            payload_schema: SCHEMA,
            fields: vec![
                field("requestId", ValueType::SignedInteger, true),
                field(
                    "evseId",
                    ValueType::UnsignedInteger,
                    evse(resource) != Some(0),
                ),
                field(
                    "chargingProfile.chargingProfilePurpose",
                    ValueType::NamedEnum,
                    false,
                ),
                field(
                    "chargingProfile.stackLevel",
                    ValueType::UnsignedInteger,
                    false,
                ),
                field(
                    "chargingProfile.chargingProfileId[]",
                    ValueType::SignedInteger,
                    false,
                ),
                field(
                    "chargingProfile.chargingLimitSource[]",
                    ValueType::NamedEnum,
                    false,
                ),
            ],
        })
        .collect()
}

fn integer(value: &Value) -> Option<i32> {
    i32::try_from(value.as_i64()?).ok()
}

/// Station scope may omit `evseId` (every EVSE), select zero (grid connection) or one EVSE;
/// EVSE scope requires its exact native ID. Criteria follow K09.FR.03.
/// # Errors
/// Rejects wrong edition/schema, `customData`, out-of-scope EVSEs and invalid criteria.
pub(crate) fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<ChargingProfilesQuery201, CommandErrorCode> {
    let invalid = CommandErrorCode::InvalidParameters;
    if operation.protocol != ProtocolEdition::Ocpp201
        || operation.action.as_str() != ACTION
        || operation.payload_schema.as_str() != SCHEMA
        || !valid_schema(REQUEST, &operation.payload)
    {
        return Err(invalid);
    }
    let payload = &operation.payload;
    let criterion = &payload["chargingProfile"];
    if payload.get("customData").is_some() || criterion.get("customData").is_some() {
        return Err(invalid);
    }
    let request_id = integer(&payload["requestId"]).ok_or(invalid)?;
    let evse_id = payload
        .get("evseId")
        .map(|value| integer(value).filter(|id| *id >= 0).ok_or(invalid))
        .transpose()?;
    match (evse(resource).ok_or(invalid)?, evse_id) {
        (0, _) => {}
        (scope, Some(id)) if scope == id => {}
        _ => return Err(invalid),
    }
    let stack_level = criterion
        .get("stackLevel")
        .map(|value| integer(value).filter(|level| *level >= 0).ok_or(invalid))
        .transpose()?;
    let charging_profile_purpose = criterion
        .get("chargingProfilePurpose")
        .map(ReportedChargingProfilePurpose201::deserialize)
        .transpose()
        .map_err(|_| invalid)?;
    let charging_profile_id = match criterion.get("chargingProfileId") {
        None => Vec::new(),
        Some(ids) => ids
            .as_array()
            .filter(|ids| ids.len() <= MAX_PROFILE_IDS)
            .ok_or(invalid)?
            .iter()
            .map(|id| integer(id).ok_or(invalid))
            .collect::<Result<Vec<_>, _>>()?,
    };
    let charging_limit_source = criterion
        .get("chargingLimitSource")
        .map(Vec::<ChargingLimitSource201>::deserialize)
        .transpose()
        .map_err(|_| invalid)?
        .unwrap_or_default();
    let mut unique = charging_limit_source.clone();
    unique.sort_unstable();
    unique.dedup();
    let filtered = stack_level.is_some()
        || charging_profile_purpose.is_some()
        || !charging_limit_source.is_empty();
    // K09.FR.03: either profile IDs or at least one other criterion, never both or neither.
    if charging_profile_id.is_empty() != filtered || unique.len() != charging_limit_source.len() {
        return Err(invalid);
    }
    GetChargingProfilesRequest::deserialize(payload).map_err(|_| invalid)?;
    Ok(ChargingProfilesQuery201 {
        request_id,
        evse_id,
        charging_profile_purpose,
        stack_level,
        charging_profile_id,
        charging_limit_source,
    })
}
