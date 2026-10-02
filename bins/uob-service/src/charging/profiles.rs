use crate::configuration::charging::StationControlOptions;
use uob_contracts::{
    NativeProtocolReference, Operation, ProtocolEdition, StationSnapshot, SupportedOperation,
};

pub(super) fn apply(
    snapshot: &mut StationSnapshot,
    protocol: ProtocolEdition,
    options: StationControlOptions,
) {
    if protocol != ProtocolEdition::Ocpp16j {
        return;
    }
    for (enabled, action) in [
        (options.set_charging_profile.enabled(), "SetChargingProfile"),
        (
            options.clear_charging_profile.enabled(),
            "ClearChargingProfile",
        ),
    ] {
        if !enabled {
            continue;
        }
        let operation = SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol,
                action: action.to_owned(),
            },
            parameters: vec![],
        };
        snapshot.capabilities.operations.push(operation.clone());
        for entry in &mut snapshot.resources {
            if matches!(entry.resource.native_protocol_reference, Some(NativeProtocolReference::Ocpp16 { connector_id }) if connector_id > 0 && i32::try_from(connector_id).is_ok())
            {
                entry.capabilities.operations.push(operation.clone());
            }
        }
    }
}
