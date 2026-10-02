use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::Value;
use uob_contracts::{
    ChargingProfile16, ChargingProfileKind16, ChargingProfilePurpose16,
    ChargingProfileRecurrency16, ClearChargingProfileRequest16, CommandErrorCode, Operation,
    PrivilegedOcppOperation, ProtocolEdition, ResourceRef, SetChargingProfileRequest16,
    StationSnapshot, UtcTimestamp, ValueType,
};

use super::{
    CommandSchemaDescriptor, CommandSchemaField, charging_profile16_schedule, composite_schedule16,
};

pub(crate) const ACTIONS: [&str; 2] = ["SetChargingProfile", "ClearChargingProfile"];
pub(crate) const SET_SCHEMA: &str = "urn:OCPP:1.6:2019:12:SetChargingProfileRequest";
pub(crate) const CLEAR_SCHEMA: &str = "urn:OCPP:1.6:2019:12:ClearChargingProfileRequest";
const SCHEMAS: [&str; 4] = [
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/1.6/SetChargingProfileRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/1.6/SetChargingProfileResponse.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/1.6/ClearChargingProfileRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/1.6/ClearChargingProfileResponse.json"
    ),
];

pub(crate) fn valid_schema(index: usize, payload: &Value) -> bool {
    static VALIDATORS: LazyLock<Vec<jsonschema::Validator>> = LazyLock::new(|| {
        SCHEMAS
            .iter()
            .map(|source| {
                let mut schema: Value = serde_json::from_str(source).expect("pinned native schema");
                // Exact lexemes enforce tenths without floating-point multipleOf arithmetic.
                // The attributed source bytes remain unmodified on disk.
                if let Some(schedule) = schema.pointer_mut(
                    "/properties/csChargingProfiles/properties/chargingSchedule/properties",
                ) {
                    schedule["minChargingRate"]
                        .as_object_mut()
                        .expect("rate schema")
                        .remove("multipleOf");
                    schedule["chargingSchedulePeriod"]["items"]["properties"]["limit"]
                        .as_object_mut()
                        .expect("rate schema")
                        .remove("multipleOf");
                }
                jsonschema::options()
                    .should_validate_formats(true)
                    .build(&schema)
                    .expect("pinned native schema")
            })
            .collect()
    });
    VALIDATORS[index].is_valid(payload)
}

pub(crate) enum Request {
    Set(SetChargingProfileRequest16),
    Clear(ClearChargingProfileRequest16),
}

fn integer(value: &Value) -> Option<i32> {
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

pub(crate) fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<Request, CommandErrorCode> {
    let index = ACTIONS
        .iter()
        .position(|action| *action == operation.action.as_str())
        .ok_or(CommandErrorCode::UnsupportedOperation)?;
    if operation.protocol != ProtocolEdition::Ocpp16j
        || operation.payload_schema.as_str() != [SET_SCHEMA, CLEAR_SCHEMA][index]
        || (index == 0
            && operation.payload["csChargingProfiles"]["chargingSchedule"]["chargingSchedulePeriod"]
                .as_array()
                .is_none_or(|periods| periods.is_empty() || periods.len() > charging_profile16_schedule::MAX_PERIODS))
        || !valid_schema(index * 2, &operation.payload)
    {
        return Err(CommandErrorCode::InvalidParameters);
    }

    let connector =
        composite_schedule16::connector(resource).ok_or(CommandErrorCode::InvalidParameters)?;
    let request = if index == 0 {
        parse_set(connector, &operation.payload).map(Request::Set)
    } else {
        parse_clear(connector, &operation.payload).map(Request::Clear)
    };
    request.ok_or(CommandErrorCode::InvalidParameters)
}

fn parse_set(connector: i32, payload: &Value) -> Option<SetChargingProfileRequest16> {
    let connector_id = integer(&payload["connectorId"])?;
    if connector_id != connector {
        return None;
    }

    let profile = &payload["csChargingProfiles"];
    let charging_profile_id = integer(&profile["chargingProfileId"])?;
    let transaction_id = optional_integer(profile.get("transactionId")).ok()?;
    let stack_level = integer(&profile["stackLevel"])?;
    let purpose = ChargingProfilePurpose16::deserialize(&profile["chargingProfilePurpose"]).ok()?;
    let kind = ChargingProfileKind16::deserialize(&profile["chargingProfileKind"]).ok()?;
    let recurrency_kind = profile
        .get("recurrencyKind")
        .map(ChargingProfileRecurrency16::deserialize)
        .transpose()
        .ok()?;
    if stack_level < 0
        || (kind != ChargingProfileKind16::Recurring && recurrency_kind.is_some())
        || (purpose == ChargingProfilePurpose16::TxProfile) != transaction_id.is_some()
        || (purpose == ChargingProfilePurpose16::TxProfile && connector <= 0)
        || (purpose == ChargingProfilePurpose16::ChargePointMaxProfile && connector != 0)
    {
        return None;
    }

    let valid_from = timestamp(profile.get("validFrom")).ok()?;
    let valid_to = timestamp(profile.get("validTo")).ok()?;
    let native = charging_profile16_schedule::decode(payload)?;
    let charging_schedule = charging_profile16_schedule::parse(
        &profile["chargingSchedule"],
        native.cs_charging_profiles.charging_schedule,
    )?;
    Some(SetChargingProfileRequest16 {
        connector_id,
        cs_charging_profiles: ChargingProfile16 {
            charging_profile_id,
            transaction_id,
            stack_level,
            charging_profile_purpose: purpose,
            charging_profile_kind: kind,
            recurrency_kind,
            valid_from,
            valid_to,
            charging_schedule,
        },
    })
}

fn parse_clear(connector: i32, payload: &Value) -> Option<ClearChargingProfileRequest16> {
    let id = optional_integer(payload.get("id")).ok()?;
    let connector_id = optional_integer(payload.get("connectorId")).ok()?;
    let stack_level = optional_integer(payload.get("stackLevel")).ok()?;
    let charging_profile_purpose = payload
        .get("chargingProfilePurpose")
        .map(ChargingProfilePurpose16::deserialize)
        .transpose()
        .ok()?;
    if connector_id.is_some_and(|id| id < 0)
        || stack_level.is_some_and(|level| level < 0)
        || (connector > 0 && (id.is_some() || connector_id != Some(connector)))
    {
        return None;
    }

    rust_ocpp::v1_6::messages::clear_charging_profile::ClearChargingProfileRequest::deserialize(
        payload,
    )
    .ok()?;
    Some(ClearChargingProfileRequest16 {
        id,
        connector_id,
        charging_profile_purpose,
        stack_level,
    })
}

pub(crate) fn descriptors(snapshot: &StationSnapshot) -> Vec<CommandSchemaDescriptor> {
    let mut result = Vec::new();
    for (index, action) in ACTIONS.iter().enumerate() {
        let required = Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: (*action).to_owned(),
        };
        let resources = std::iter::once((&snapshot.station, &snapshot.capabilities)).chain(
            snapshot
                .resources
                .iter()
                .map(|entry| (&entry.resource, &entry.capabilities)),
        );
        for (resource, capabilities) in resources {
            if resource.bridge_id != snapshot.station.bridge_id
                || resource.station_id != snapshot.station.station_id
                || composite_schedule16::connector(resource).is_none()
                || !capabilities.supports(&required)
            {
                continue;
            }
            let fields = if index == 0 {
                set_fields()
            } else {
                vec![
                    field("id", ValueType::SignedInteger, false),
                    field("connectorId", ValueType::SignedInteger, false),
                    field("chargingProfilePurpose", ValueType::NamedEnum, false),
                    field("stackLevel", ValueType::SignedInteger, false),
                ]
            };
            result.push(CommandSchemaDescriptor {
                resource: resource.clone(),
                protocol: ProtocolEdition::Ocpp16j,
                action,
                payload_schema: [SET_SCHEMA, CLEAR_SCHEMA][index],
                fields,
            });
        }
    }
    result
}

fn set_fields() -> Vec<CommandSchemaField> {
    vec![
        field("connectorId", ValueType::SignedInteger, true),
        field(
            "csChargingProfiles.chargingProfileId",
            ValueType::SignedInteger,
            true,
        ),
        field(
            "csChargingProfiles.transactionId",
            ValueType::SignedInteger,
            false,
        ),
        field(
            "csChargingProfiles.stackLevel",
            ValueType::SignedInteger,
            true,
        ),
        field(
            "csChargingProfiles.chargingProfilePurpose",
            ValueType::NamedEnum,
            true,
        ),
        field(
            "csChargingProfiles.chargingProfileKind",
            ValueType::NamedEnum,
            true,
        ),
        field(
            "csChargingProfiles.recurrencyKind",
            ValueType::NamedEnum,
            false,
        ),
        field("csChargingProfiles.validFrom", ValueType::Text, false),
        field("csChargingProfiles.validTo", ValueType::Text, false),
        field(
            "csChargingProfiles.chargingSchedule.duration",
            ValueType::SignedInteger,
            false,
        ),
        field(
            "csChargingProfiles.chargingSchedule.startSchedule",
            ValueType::Text,
            false,
        ),
        field(
            "csChargingProfiles.chargingSchedule.chargingRateUnit",
            ValueType::NamedEnum,
            true,
        ),
        field(
            "csChargingProfiles.chargingSchedule.chargingSchedulePeriod[].startPeriod",
            ValueType::SignedInteger,
            true,
        ),
        field(
            "csChargingProfiles.chargingSchedule.chargingSchedulePeriod[].limit",
            ValueType::Decimal,
            true,
        ),
        field(
            "csChargingProfiles.chargingSchedule.chargingSchedulePeriod[].numberPhases",
            ValueType::SignedInteger,
            false,
        ),
        field(
            "csChargingProfiles.chargingSchedule.minChargingRate",
            ValueType::Decimal,
            false,
        ),
    ]
}

fn field(name: &'static str, value_type: ValueType, required: bool) -> CommandSchemaField {
    let enum_values = match name.rsplit('.').next() {
        Some("chargingProfilePurpose") => Some(vec![
            "ChargePointMaxProfile",
            "TxDefaultProfile",
            "TxProfile",
        ]),
        Some("chargingProfileKind") => Some(vec!["Absolute", "Recurring", "Relative"]),
        Some("recurrencyKind") => Some(vec!["Daily", "Weekly"]),
        Some("chargingRateUnit") => Some(vec!["A", "W"]),
        _ => None,
    };
    CommandSchemaField {
        name,
        value_type,
        required,
        enum_values,
    }
}
