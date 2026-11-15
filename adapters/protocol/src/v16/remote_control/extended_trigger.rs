//! OCPP 1.6 Security Whitepaper `ExtendedTriggerMessage`: native payload and frozen targets.
//! The reply is the plain `TriggerMessage` reply and is mapped by `trigger::response`.
use super::trigger;
use crate::command_registry::extended_trigger16;
use serde_json::Value;
use uob_application::TriggerExpectation16;
use uob_contracts::{CommandErrorCode, PrivilegedOcppOperation, ResourceRef, StationSnapshot};

pub(super) use extended_trigger16::ACTION;

/// Validates, gates the class on the station's advertised capability and freezes the targets.
pub(super) fn prepare(
    operation: &PrivilegedOcppOperation<Value>,
    resource: &ResourceRef,
    snapshot: &StationSnapshot,
) -> Result<(Value, TriggerExpectation16), CommandErrorCode> {
    let request = extended_trigger16::validate(resource, operation)?;
    if !extended_trigger16::class_available(&snapshot.capabilities, request.class) {
        return Err(CommandErrorCode::UnsupportedOperation);
    }
    let expectation =
        trigger::expectation(request.class, request.connector_id, resource, snapshot)?;
    Ok((operation.payload.clone(), expectation))
}
