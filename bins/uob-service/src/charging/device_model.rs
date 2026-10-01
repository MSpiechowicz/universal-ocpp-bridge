use crate::configuration::charging::StationControlOptions;
use uob_contracts::{
    CanonicalResource, NativeProtocolReference, Operation, ProtocolEdition, StationSnapshot,
    SupportedOperation,
};

pub(super) fn apply(
    snapshot: &mut StationSnapshot,
    protocol: ProtocolEdition,
    options: StationControlOptions,
) {
    if protocol != ProtocolEdition::Ocpp201 {
        return;
    }
    let enabled = [
        ("GetVariables", options.get_variables.enabled()),
        ("GetBaseReport", options.get_base_report.enabled()),
        ("GetReport", options.get_report.enabled()),
    ];
    for (action, enabled) in enabled {
        if !enabled {
            continue;
        }
        let operation = SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol,
                action: action.to_owned(),
            },
            parameters: Vec::new(),
        };
        snapshot.capabilities.operations.push(operation.clone());
        if action == "GetBaseReport" {
            continue;
        }
        for entry in &mut snapshot.resources {
            if matches!(
                (
                    &entry.resource.resource,
                    entry.resource.native_protocol_reference
                ),
                (
                    Some(CanonicalResource::Evse { .. }),
                    Some(NativeProtocolReference::Ocpp201 { evse_id: 1.., .. })
                )
            ) {
                entry.capabilities.operations.push(operation.clone());
            }
        }
    }
}
