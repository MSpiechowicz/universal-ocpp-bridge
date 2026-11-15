//! OCPP 1.6 Security Whitepaper `ExtendedTriggerMessage`. The station root asks for a message
//! the station owns; a connector asks for one of its own, so both advertise the action.
use crate::configuration::charging::StationControlOptions;
use uob_contracts::{
    NativeProtocolReference, Operation, ProtocolEdition, StationSnapshot, SupportedOperation,
};

const ACTION: &str = "ExtendedTriggerMessage";

fn operation() -> SupportedOperation {
    SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: ACTION.to_owned(),
        },
        parameters: Vec::new(),
    }
}

pub(super) fn apply(
    snapshot: &mut StationSnapshot,
    protocol: ProtocolEdition,
    options: StationControlOptions,
) {
    if protocol != ProtocolEdition::Ocpp16j || !options.extended_trigger_message.enabled() {
        return;
    }

    snapshot.capabilities.operations.push(operation());
    for entry in &mut snapshot.resources {
        if matches!(
            entry.resource.native_protocol_reference,
            Some(NativeProtocolReference::Ocpp16 { connector_id: 1.. })
        ) {
            entry.capabilities.operations.push(operation());
        }
    }
}
