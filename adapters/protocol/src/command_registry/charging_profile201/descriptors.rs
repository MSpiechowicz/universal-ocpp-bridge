use super::{ACTIONS, CLEAR_SCHEMA, SET_SCHEMA, evse};
use crate::command_registry::{CommandSchemaDescriptor, CommandSchemaField};
use uob_contracts::{Operation, ProtocolEdition, StationSnapshot, ValueType};

pub(crate) fn descriptors(snapshot: &StationSnapshot) -> Vec<CommandSchemaDescriptor> {
    let mut result = Vec::new();
    for (index, action) in ACTIONS.iter().enumerate() {
        let required = Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp201,
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
                || evse(resource).is_none()
                || !capabilities.supports(&required)
            {
                continue;
            }
            result.push(CommandSchemaDescriptor {
                resource: resource.clone(),
                protocol: ProtocolEdition::Ocpp201,
                action,
                payload_schema: [SET_SCHEMA, CLEAR_SCHEMA][index],
                fields: if index == 0 {
                    set_fields()
                } else {
                    clear_fields()
                },
            });
        }
    }
    result
}
fn field(name: &'static str, value_type: ValueType, required: bool) -> CommandSchemaField {
    let values = if name.ends_with("chargingProfilePurpose") {
        Some(vec![
            "ChargingStationMaxProfile",
            "TxDefaultProfile",
            "TxProfile",
        ])
    } else if name.ends_with("chargingProfileKind") {
        Some(vec!["Absolute", "Recurring", "Relative"])
    } else if name.ends_with("recurrencyKind") {
        Some(vec!["Daily", "Weekly"])
    } else if name.ends_with("chargingRateUnit") {
        Some(vec!["A", "W"])
    } else {
        None
    };
    CommandSchemaField {
        name,
        value_type,
        required,
        enum_values: values,
    }
}
fn clear_fields() -> Vec<CommandSchemaField> {
    vec![
        field("chargingProfileId", ValueType::SignedInteger, false),
        field(
            "chargingProfileCriteria.evseId",
            ValueType::SignedInteger,
            false,
        ),
        field(
            "chargingProfileCriteria.chargingProfilePurpose",
            ValueType::NamedEnum,
            false,
        ),
        field(
            "chargingProfileCriteria.stackLevel",
            ValueType::SignedInteger,
            false,
        ),
    ]
}
fn set_fields() -> Vec<CommandSchemaField> {
    vec![
        field("evseId", ValueType::SignedInteger, true),
        field("chargingProfile.id", ValueType::SignedInteger, true),
        field("chargingProfile.transactionId", ValueType::Text, false),
        field("chargingProfile.stackLevel", ValueType::SignedInteger, true),
        field(
            "chargingProfile.chargingProfilePurpose",
            ValueType::NamedEnum,
            true,
        ),
        field(
            "chargingProfile.chargingProfileKind",
            ValueType::NamedEnum,
            true,
        ),
        field(
            "chargingProfile.recurrencyKind",
            ValueType::NamedEnum,
            false,
        ),
        field("chargingProfile.validFrom", ValueType::Text, false),
        field("chargingProfile.validTo", ValueType::Text, false),
        field(
            "chargingProfile.chargingSchedule[].id",
            ValueType::SignedInteger,
            true,
        ),
        field(
            "chargingProfile.chargingSchedule[].duration",
            ValueType::SignedInteger,
            false,
        ),
        field(
            "chargingProfile.chargingSchedule[].startSchedule",
            ValueType::Text,
            false,
        ),
        field(
            "chargingProfile.chargingSchedule[].chargingRateUnit",
            ValueType::NamedEnum,
            true,
        ),
        field(
            "chargingProfile.chargingSchedule[].minChargingRate",
            ValueType::Decimal,
            false,
        ),
        field(
            "chargingProfile.chargingSchedule[].chargingSchedulePeriod[].startPeriod",
            ValueType::SignedInteger,
            true,
        ),
        field(
            "chargingProfile.chargingSchedule[].chargingSchedulePeriod[].limit",
            ValueType::Decimal,
            true,
        ),
        field(
            "chargingProfile.chargingSchedule[].chargingSchedulePeriod[].numberPhases",
            ValueType::SignedInteger,
            false,
        ),
        field(
            "chargingProfile.chargingSchedule[].chargingSchedulePeriod[].phaseToUse",
            ValueType::SignedInteger,
            false,
        ),
    ]
}
