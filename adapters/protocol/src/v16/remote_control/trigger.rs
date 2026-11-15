//! OCPP 1.6 `TriggerMessage` scope and immutable native-target snapshot, plus the routing both
//! trigger actions share.
use super::extended_trigger;
use rust_ocpp::v1_6::messages::trigger_message::{TriggerMessageRequest, TriggerMessageResponse};
use rust_ocpp::v1_6::types::MessageTrigger;
use serde_json::Value;
use uob_application::{CommandDispatchOutcome, TriggerExpectation16};
use uob_contracts::{
    CanonicalResource, CommandErrorCode, NativeProtocolReference, PrivilegedOcppOperation,
    ResourceRef, StationSnapshot, TriggerMessageClass, TriggerNativeResponse,
};

pub(super) const SCHEMA: &str = "urn:OCPP:1.6:2019:12:TriggerMessageRequest";

/// Whether the operation asks the station to send one of its own messages.
pub(super) fn is_trigger(operation: &PrivilegedOcppOperation<Value>) -> bool {
    matches!(
        operation.action.as_str(),
        "TriggerMessage" | extended_trigger::ACTION
    )
}

/// Prepares either trigger action; returns the native action name, payload and frozen targets.
pub(super) fn prepare_operation(
    operation: &PrivilegedOcppOperation<Value>,
    resource: &ResourceRef,
    snapshot: &StationSnapshot,
) -> Result<(&'static str, Value, TriggerExpectation16), CommandErrorCode> {
    if operation.action.as_str() == extended_trigger::ACTION {
        return extended_trigger::prepare(operation, resource, snapshot)
            .map(|(payload, expectation)| (extended_trigger::ACTION, payload, expectation));
    }
    if operation.payload_schema.as_str() != SCHEMA {
        return Err(CommandErrorCode::InvalidParameters);
    }
    prepare(operation, resource, snapshot)
        .map(|(payload, expectation)| ("TriggerMessage", payload, expectation))
}

pub(super) fn prepare(
    operation: &PrivilegedOcppOperation<Value>,
    resource: &ResourceRef,
    snapshot: &StationSnapshot,
) -> Result<(Value, TriggerExpectation16), CommandErrorCode> {
    use CommandErrorCode::InvalidParameters;
    crate::command_registry::validate_privileged_operation(resource, operation)?;
    let request: TriggerMessageRequest =
        serde_json::from_value(operation.payload.clone()).map_err(|_| InvalidParameters)?;
    let class = match request.requested_message {
        MessageTrigger::BootNotification => TriggerMessageClass::BootNotification,
        MessageTrigger::DiagnosticsStatusNotification => {
            TriggerMessageClass::DiagnosticsStatusNotification
        }
        MessageTrigger::FirmwareStatusNotification => {
            TriggerMessageClass::FirmwareStatusNotification
        }
        MessageTrigger::Heartbeat => TriggerMessageClass::Heartbeat,
        MessageTrigger::MeterValues => TriggerMessageClass::MeterValues,
        MessageTrigger::StatusNotification => TriggerMessageClass::StatusNotification,
    };
    let expectation = expectation(class, request.connector_id, resource, snapshot)?;
    Ok((operation.payload.clone(), expectation))
}

/// Freezes the native targets of one trigger request; shared by `TriggerMessage` and
/// `ExtendedTriggerMessage`. The caller has already validated the request against its schema.
pub(super) fn expectation(
    class: TriggerMessageClass,
    native_scope: Option<u32>,
    resource: &ResourceRef,
    snapshot: &StationSnapshot,
) -> Result<TriggerExpectation16, CommandErrorCode> {
    use CommandErrorCode::InvalidParameters;
    let expected_targets = if class.is_station_only() {
        vec![0]
    } else if native_scope == Some(0) {
        if class != TriggerMessageClass::StatusNotification || resource != &snapshot.station {
            return Err(InvalidParameters);
        }
        vec![0]
    } else if let Some(id) = native_scope {
        if connector_id(resource) != Some(id)
            || !snapshot
                .resources
                .iter()
                .any(|entry| entry.resource == *resource)
        {
            return Err(InvalidParameters);
        }
        vec![id]
    } else {
        if resource != &snapshot.station {
            return Err(InvalidParameters);
        }
        let mut targets = snapshot
            .resources
            .iter()
            .filter_map(|entry| {
                (entry.resource.bridge_id == snapshot.station.bridge_id
                    && entry.resource.station_id == snapshot.station.station_id)
                    .then(|| connector_id(&entry.resource))
                    .flatten()
            })
            .collect::<Vec<_>>();
        targets.sort_unstable();
        if targets.len() > 64 || targets.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(InvalidParameters);
        }
        if class == TriggerMessageClass::StatusNotification {
            targets.insert(0, 0);
        } else if targets.is_empty() {
            return Err(InvalidParameters);
        }
        targets
    };
    Ok(TriggerExpectation16 {
        requested_class: class,
        native_scope,
        expected_targets,
    })
}

fn connector_id(resource: &ResourceRef) -> Option<u32> {
    match (&resource.resource, resource.native_protocol_reference) {
        (
            Some(CanonicalResource::Connector { .. }),
            Some(NativeProtocolReference::Ocpp16 { connector_id: id }),
        ) if id > 0 => Some(id),
        _ => None,
    }
}

pub(super) fn response(payload: &Value) -> CommandDispatchOutcome {
    let Some(object) = payload.as_object() else {
        return super::mapping::uncertain();
    };
    if object.len() != 1 || !object.contains_key("status") {
        return super::mapping::uncertain();
    }
    if serde_json::from_value::<TriggerMessageResponse>(payload.clone()).is_err() {
        return super::mapping::uncertain();
    }
    match serde_json::from_value::<TriggerNativeResponse>(payload["status"].clone()) {
        Ok(status) => CommandDispatchOutcome::TriggerResponse(status),
        Err(_) => super::mapping::uncertain(),
    }
}
