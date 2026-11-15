//! Management command-schema listing for the monitoring actions.
use super::{ACTIONS, payload_schema};
use crate::command_registry::{
    CommandSchemaDescriptor, CommandSchemaField, firmware201::station_scope,
};
use uob_contracts::{Operation, ProtocolEdition, StationSnapshot, ValueType};

const MONITOR_TYPES: [&str; 5] = [
    "UpperThreshold",
    "LowerThreshold",
    "Delta",
    "Periodic",
    "PeriodicClockAligned",
];

fn field(name: &'static str, value_type: ValueType, required: bool) -> CommandSchemaField {
    let enum_values = match name {
        "setMonitoringData[].type" => Some(MONITOR_TYPES.to_vec()),
        "monitoringCriteria[]" => Some(vec![
            "ThresholdMonitoring",
            "DeltaMonitoring",
            "PeriodicMonitoring",
        ]),
        "monitoringBase" => Some(vec!["All", "FactoryDefault", "HardWiredOnly"]),
        _ => None,
    };

    CommandSchemaField {
        name,
        value_type,
        required,
        enum_values,
    }
}

fn fields(index: usize) -> Vec<CommandSchemaField> {
    use ValueType::{Boolean, Decimal, NamedEnum, SignedInteger, Text, UnsignedInteger};

    match index {
        0 => vec![
            field("setMonitoringData[].id", SignedInteger, false),
            field("setMonitoringData[].transaction", Boolean, false),
            field("setMonitoringData[].value", Decimal, true),
            field("setMonitoringData[].type", NamedEnum, true),
            field("setMonitoringData[].severity", UnsignedInteger, true),
            field("setMonitoringData[].component.name", Text, true),
            field("setMonitoringData[].component.instance", Text, false),
            field(
                "setMonitoringData[].component.evse.id",
                SignedInteger,
                false,
            ),
            field(
                "setMonitoringData[].component.evse.connectorId",
                SignedInteger,
                false,
            ),
            field("setMonitoringData[].variable.name", Text, true),
            field("setMonitoringData[].variable.instance", Text, false),
        ],
        1 => vec![field("id[]", SignedInteger, true)],
        2 => vec![
            field("requestId", SignedInteger, true),
            field("monitoringCriteria[]", NamedEnum, false),
            field("componentVariable[].component.name", Text, false),
            field("componentVariable[].component.instance", Text, false),
            field(
                "componentVariable[].component.evse.id",
                SignedInteger,
                false,
            ),
            field(
                "componentVariable[].component.evse.connectorId",
                SignedInteger,
                false,
            ),
            field("componentVariable[].variable.name", Text, false),
            field("componentVariable[].variable.instance", Text, false),
        ],
        3 => vec![field("monitoringBase", NamedEnum, true)],
        _ => vec![field("severity", UnsignedInteger, true)],
    }
}

/// Offered only on the station root and only when explicitly advertised.
pub(crate) fn descriptors(snapshot: &StationSnapshot) -> Vec<CommandSchemaDescriptor> {
    if !station_scope(&snapshot.station) {
        return Vec::new();
    }

    ACTIONS
        .iter()
        .enumerate()
        .filter(|(_, action)| {
            snapshot.capabilities.supports(&Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp201,
                action: (**action).to_owned(),
            })
        })
        .map(|(index, action)| CommandSchemaDescriptor {
            resource: snapshot.station.clone(),
            protocol: ProtocolEdition::Ocpp201,
            action,
            payload_schema: payload_schema(index),
            fields: fields(index),
        })
        .collect()
}
