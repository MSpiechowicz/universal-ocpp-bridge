pub(super) mod supervisor;
use crate::configuration::charging::StationControlOptions;
use uob_contracts::{
    NativeProtocolReference, Operation, ProtocolEdition, StationSnapshot, SupportedOperation,
};

pub(super) fn apply(
    snapshot: &mut StationSnapshot,
    protocol: ProtocolEdition,
    options: StationControlOptions,
) {
    let edition201 = protocol == ProtocolEdition::Ocpp201;
    for (enabled, action) in [
        (options.set_charging_profile.enabled(), "SetChargingProfile"),
        (
            options.clear_charging_profile.enabled(),
            "ClearChargingProfile",
        ),
        // The station-level 2.0.1 composite schedule is advertised with the other station actions.
        (
            options.get_composite_schedule.enabled() && edition201,
            "GetCompositeSchedule",
        ),
        (
            options.get_charging_profiles.enabled() && edition201,
            "GetChargingProfiles",
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
        if action != "GetCompositeSchedule" {
            snapshot.capabilities.operations.push(operation.clone());
        }
        for entry in &mut snapshot.resources {
            let supported = match (protocol, entry.resource.native_protocol_reference) {
                (
                    ProtocolEdition::Ocpp16j,
                    Some(NativeProtocolReference::Ocpp16 { connector_id }),
                ) => connector_id > 0 && i32::try_from(connector_id).is_ok(),
                (
                    ProtocolEdition::Ocpp201,
                    Some(NativeProtocolReference::Ocpp201 {
                        evse_id,
                        connector_id: None,
                    }),
                ) => {
                    matches!(
                        entry.resource.resource,
                        Some(uob_contracts::CanonicalResource::Evse {
                            connector_id: None,
                            ..
                        })
                    ) && evse_id > 0
                        && i32::try_from(evse_id).is_ok()
                }
                _ => false,
            };
            if supported {
                entry.capabilities.operations.push(operation.clone());
            }
        }
    }
}
