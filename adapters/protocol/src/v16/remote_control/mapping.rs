use super::{RemoteStartIdentity, charging_limit, trigger};
use super::{configuration, configuration_values::LocalConfigurationValues};
use rust_ocpp::v1_6::messages::{
    change_availability::{ChangeAvailabilityRequest, ChangeAvailabilityResponse},
    remote_start_transaction::{RemoteStartTransactionRequest, RemoteStartTransactionResponse},
    remote_stop_transaction::{RemoteStopTransactionRequest, RemoteStopTransactionResponse},
    reset::{ResetRequest, ResetResponse},
    unlock_connector::{UnlockConnectorRequest, UnlockConnectorResponse},
};
use serde_json::Value;
use uob_application::{CommandDispatchOutcome, TriggerExpectation};
use uob_contracts::{
    AvailabilityState, Command, CommandError, CommandErrorCode, CommandOperation, Connectivity,
    NativeProtocolReference, ProtocolEdition, ResourceCapabilities, ResourceRef, StationSnapshot,
    TransactionState, UtcTimestamp,
};
use validator::Validate;

pub(super) fn capabilities<'a>(
    snapshot: &'a StationSnapshot,
    resource: &ResourceRef,
) -> Option<&'a ResourceCapabilities> {
    if resource == &snapshot.station {
        return Some(&snapshot.capabilities);
    }
    snapshot
        .resources
        .iter()
        .find(|entry| &entry.resource == resource)
        .map(|entry| &entry.capabilities)
}

pub(super) fn prepare(
    command: &Command<Value>,
    snapshot: &StationSnapshot,
    identity: &dyn RemoteStartIdentity,
    configuration_values: Option<&LocalConfigurationValues>,
    facts: &configuration::SessionFacts,
    now: UtcTimestamp,
    profile_request: &mut Option<crate::command_registry::charging_profile16::Request>,
) -> Result<(&'static str, Value), CommandErrorCode> {
    use CommandErrorCode::{InvalidParameters, PolicyRejected, UnsupportedOperation};
    if !matches!(
        snapshot.connectivity,
        Connectivity::Connected {
            protocol: ProtocolEdition::Ocpp16j,
            ..
        }
    ) {
        return Err(CommandErrorCode::StationDisconnected);
    }
    if !boot_trigger(command) {
        uob_application::registration::accepted(snapshot).map_err(|_| PolicyRejected)?;
    } else if uob_application::registration::accepted(snapshot).is_ok() {
        return Err(PolicyRejected);
    }
    let capabilities = capabilities(snapshot, &command.resource).ok_or(InvalidParameters)?;
    command
        .validate_for_dispatch(capabilities, now)
        .map_err(|error| match error {
            uob_contracts::CommandValidationError::Expired => CommandErrorCode::Expired,
            uob_contracts::CommandValidationError::UnsupportedOperation(_) => UnsupportedOperation,
        })?;
    super::constraints::validate(
        command,
        capabilities
            .require(&command.operation.required_capability())
            .map_err(|_| UnsupportedOperation)?,
    )?;
    let native = match command.resource.native_protocol_reference {
        Some(NativeProtocolReference::Ocpp16 { connector_id }) => connector_id,
        None if command.resource == snapshot.station && command.resource.resource.is_none() => 0,
        _ => return Err(InvalidParameters),
    };
    match &command.operation {
        CommandOperation::Start {
            authorization_reference,
        } => prepare_start(
            command,
            snapshot,
            identity,
            authorization_reference.as_deref(),
            native,
            now,
        ),
        CommandOperation::Stop { transaction_id } => {
            prepare_stop(command, snapshot, transaction_id)
        }
        CommandOperation::SetChargingLimit(limit) => Ok((
            "SetChargingProfile",
            charging_limit::prepare(command, snapshot, limit)?,
        )),
        CommandOperation::Ocpp(operation) if operation.protocol == ProtocolEdition::Ocpp16j => {
            if crate::command_registry::reservation16::ACTIONS.contains(&operation.action.as_str())
            {
                crate::command_registry::reservation16::validate(&command.resource, operation)?;
                let action = if operation.action.as_str() == "ReserveNow" {
                    "ReserveNow"
                } else {
                    "CancelReservation"
                };
                return Ok((action, operation.payload.clone()));
            }
            if crate::command_registry::charging_profile16::ACTIONS
                .contains(&operation.action.as_str())
            {
                let request =
                    super::charging_profile::prepare(&command.resource, operation, snapshot)?;
                let action = match &request {
                    crate::command_registry::charging_profile16::Request::Set(_) => {
                        "SetChargingProfile"
                    }
                    crate::command_registry::charging_profile16::Request::Clear(_) => {
                        "ClearChargingProfile"
                    }
                };
                *profile_request = Some(request);
                return Ok((action, operation.payload.clone()));
            }
            if operation.action.as_str() == "TriggerMessage" {
                if operation.payload_schema.as_str() != trigger::SCHEMA {
                    return Err(InvalidParameters);
                }
                return trigger::prepare(operation, &command.resource, snapshot)
                    .map(|(payload, _)| ("TriggerMessage", payload));
            }
            privileged(
                operation,
                &command.resource,
                &snapshot.station,
                native,
                configuration_values,
                facts,
                now,
            )
        }
        CommandOperation::Ocpp(_) => Err(UnsupportedOperation),
    }
}

fn prepare_start(
    command: &Command<Value>,
    snapshot: &StationSnapshot,
    identity: &dyn RemoteStartIdentity,
    authorization_reference: Option<&str>,
    native: u32,
    now: UtcTimestamp,
) -> Result<(&'static str, Value), CommandErrorCode> {
    use CommandErrorCode::{InvalidParameters, PolicyRejected};
    if !available(snapshot, &command.resource) {
        return Err(PolicyRejected);
    }
    let reference = authorization_reference.ok_or(PolicyRejected)?;
    let token = identity
        .authorized_token(reference, &command.resource, now)
        .ok_or(PolicyRejected)?;
    let id_tag = std::str::from_utf8(token.expose_to_provider())
        .map_err(|_| InvalidParameters)?
        .to_owned();
    let request = RemoteStartTransactionRequest {
        connector_id: (native != 0).then_some(native),
        id_tag,
        charging_profile: None,
    };
    request.validate().map_err(|_| InvalidParameters)?;
    Ok(("RemoteStartTransaction", encode(request)?))
}

fn prepare_stop(
    command: &Command<Value>,
    snapshot: &StationSnapshot,
    transaction_id: &uob_contracts::TransactionId,
) -> Result<(&'static str, Value), CommandErrorCode> {
    use CommandErrorCode::InvalidParameters;
    let tx = snapshot
        .transactions
        .iter()
        .find(|tx| {
            &tx.transaction_id == transaction_id
                && (command.resource == snapshot.station || tx.resource == command.resource)
                && tx.state != TransactionState::Ended
        })
        .ok_or(InvalidParameters)?;
    let native_id = tx.ocpp16.as_ref().ok_or(InvalidParameters)?.transaction_id;
    Ok((
        "RemoteStopTransaction",
        encode(RemoteStopTransactionRequest {
            transaction_id: native_id,
        })?,
    ))
}

fn boot_trigger(command: &Command<Value>) -> bool {
    matches!(
        &command.operation,
        CommandOperation::Ocpp(operation)
            if operation.protocol == ProtocolEdition::Ocpp16j
                && operation.action.as_str() == "TriggerMessage"
                && operation.payload["requestedMessage"] == "BootNotification"
    )
}

pub(super) fn trigger_expectation(
    command: &Command<Value>,
    snapshot: &StationSnapshot,
    now: UtcTimestamp,
) -> Option<TriggerExpectation> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return None;
    };
    if operation.action.as_str() != "TriggerMessage"
        || operation.protocol != ProtocolEdition::Ocpp16j
        || operation.payload_schema.as_str() != trigger::SCHEMA
        || boot_trigger(command) == uob_application::registration::accepted(snapshot).is_ok()
    {
        return None;
    }
    let capabilities = capabilities(snapshot, &command.resource)?;
    command.validate_for_dispatch(capabilities, now).ok()?;
    super::constraints::validate(
        command,
        capabilities
            .require(&command.operation.required_capability())
            .ok()?,
    )
    .ok()?;
    trigger::prepare(operation, &command.resource, snapshot)
        .ok()
        .map(|(_, expectation)| TriggerExpectation::Ocpp16(expectation))
}

fn privileged(
    operation: &uob_contracts::PrivilegedOcppOperation<Value>,
    resource: &ResourceRef,
    station: &ResourceRef,
    native: u32,
    configuration_values: Option<&LocalConfigurationValues>,
    facts: &configuration::SessionFacts,
    now: UtcTimestamp,
) -> Result<(&'static str, Value), CommandErrorCode> {
    use CommandErrorCode::{InvalidParameters, UnsupportedOperation};
    match operation.action.as_str() {
        "GetLocalListVersion" | "SendLocalList" | "ClearCache" => {
            if resource != station || native != 0 {
                return Err(InvalidParameters);
            }
            crate::command_registry::local_authorization16::validate(resource, operation)?;
            let action = match operation.action.as_str() {
                "GetLocalListVersion" => "GetLocalListVersion",
                "SendLocalList" => "SendLocalList",
                _ => "ClearCache",
            };
            Ok((action, operation.payload.clone()))
        }
        "GetCompositeSchedule" => {
            crate::command_registry::composite_schedule16::validate(resource, operation)?;
            Ok(("GetCompositeSchedule", operation.payload.clone()))
        }
        "GetConfiguration" if operation.payload_schema.as_str() == configuration::GET_SCHEMA => {
            if resource != station || native != 0 {
                return Err(InvalidParameters);
            }
            Ok((
                "GetConfiguration",
                configuration::get_request(&operation.payload, facts.max_keys.unwrap_or(256))?,
            ))
        }
        "ChangeConfiguration"
            if operation.payload_schema.as_str() == configuration::CHANGE_REFERENCE_SCHEMA =>
        {
            if resource != station || native != 0 {
                return Err(InvalidParameters);
            }
            let key = operation
                .payload
                .get("key")
                .and_then(Value::as_str)
                .ok_or(InvalidParameters)?;
            // Once bounded session facts fill up, an unretained key may be read-only.
            if facts.readonly.get(key) == Some(&true)
                || (facts.readonly.len() == 256 && !facts.readonly.contains_key(key))
            {
                return Err(CommandErrorCode::PolicyRejected);
            }
            let provider = configuration_values.ok_or(CommandErrorCode::PolicyRejected)?;
            Ok((
                "ChangeConfiguration",
                configuration::change_request(&operation.payload, resource, provider, now)?,
            ))
        }
        "ChangeAvailability"
            if operation.payload_schema.as_str()
                == "urn:OCPP:1.6:2019:12:ChangeAvailabilityRequest" =>
        {
            if resource == station {
                crate::command_registry::validate_privileged_operation(resource, operation)?;
            }
            exact_fields(&operation.payload, &["connectorId", "type"])?;
            let request: ChangeAvailabilityRequest =
                serde_json::from_value(operation.payload.clone()).map_err(|_| InvalidParameters)?;
            // Connector zero widens control to the station and every connector. Never allow
            // a connector-scoped command to acquire that authority through its wire payload.
            if request.connector_id != native || (native == 0) != (resource == station) {
                return Err(InvalidParameters);
            }
            Ok(("ChangeAvailability", encode(request)?))
        }
        "Reset" if operation.payload_schema.as_str() == "urn:OCPP:1.6:2019:12:ResetRequest" => {
            if resource != station || native != 0 {
                return Err(InvalidParameters);
            }
            exact_fields(&operation.payload, &["type"])?;
            let request: ResetRequest =
                serde_json::from_value(operation.payload.clone()).map_err(|_| InvalidParameters)?;
            Ok(("Reset", encode(request)?))
        }
        "UnlockConnector"
            if operation.payload_schema.as_str()
                == "urn:OCPP:1.6:2019:12:UnlockConnectorRequest" =>
        {
            exact_fields(&operation.payload, &["connectorId"])?;
            let request: UnlockConnectorRequest =
                serde_json::from_value(operation.payload.clone()).map_err(|_| InvalidParameters)?;
            // The model's <=20 check is not an OCPP constraint. Match the admitted topology instead.
            if native == 0 || resource == station || request.connector_id != native {
                return Err(InvalidParameters);
            }
            Ok(("UnlockConnector", encode(request)?))
        }
        _ => Err(UnsupportedOperation),
    }
}
fn available(snapshot: &StationSnapshot, resource: &ResourceRef) -> bool {
    if snapshot.current_values.iter().any(|value| {
        value.point_id.as_str() == "ocpp16/connector-0/status/status"
            && matches!(&value.value, Some(uob_contracts::TypedValue::Text(status)) if status == "Unavailable" || status == "Faulted")
    }) {
        return false;
    }
    snapshot.resources.iter().any(|entry| {
        (resource == &snapshot.station || &entry.resource == resource)
            && entry.availability == AvailabilityState::Available
            && !snapshot
                .transactions
                .iter()
                .any(|tx| tx.resource == entry.resource && tx.state != TransactionState::Ended)
    })
}
fn exact_fields(value: &Value, names: &[&str]) -> Result<(), CommandErrorCode> {
    if value
        .as_object()
        .is_some_and(|o| o.len() == names.len() && names.iter().all(|name| o.contains_key(*name)))
    {
        Ok(())
    } else {
        Err(CommandErrorCode::InvalidParameters)
    }
}
fn encode(value: impl serde::Serialize) -> Result<Value, CommandErrorCode> {
    serde_json::to_value(value).map_err(|_| CommandErrorCode::InvalidParameters)
}
pub(super) fn response(action: &str, payload: &Value) -> CommandDispatchOutcome {
    if action == "TriggerMessage" {
        return trigger::response(payload);
    }
    if exact_fields(payload, &["status"]).is_err() {
        return uncertain();
    }
    // Decode with the action-specific pinned model; unknown statuses are not a denial or success.
    let valid = match action {
        "RemoteStartTransaction" => {
            serde_json::from_value::<RemoteStartTransactionResponse>(payload.clone()).is_ok()
        }
        "RemoteStopTransaction" => {
            serde_json::from_value::<RemoteStopTransactionResponse>(payload.clone()).is_ok()
        }
        "ChangeAvailability" => {
            serde_json::from_value::<ChangeAvailabilityResponse>(payload.clone()).is_ok()
        }
        "Reset" => serde_json::from_value::<ResetResponse>(payload.clone()).is_ok(),
        "SetChargingProfile" => charging_limit::valid_response(payload),
        "UnlockConnector" => {
            serde_json::from_value::<UnlockConnectorResponse>(payload.clone()).is_ok()
        }
        _ => false,
    };
    if !valid {
        return uncertain();
    }
    if payload["status"] == "Accepted"
        || (action == "ChangeAvailability" && payload["status"] == "Scheduled")
        || (action == "UnlockConnector" && payload["status"] == "Unlocked")
    {
        CommandDispatchOutcome::ProtocolResponse {
            accepted: true,
            error: None,
        }
    } else {
        CommandDispatchOutcome::ProtocolResponse {
            accepted: false,
            error: Some(CommandError {
                code: CommandErrorCode::ProtocolRejected,
                detail: payload["status"].as_str().map(str::to_owned),
            }),
        }
    }
}
pub(super) fn not_sent(code: CommandErrorCode) -> CommandDispatchOutcome {
    CommandDispatchOutcome::NotTransmitted {
        error: CommandError { code, detail: None },
    }
}
pub(super) fn rejected_response() -> CommandDispatchOutcome {
    CommandDispatchOutcome::ProtocolResponse {
        accepted: false,
        error: Some(CommandError {
            code: CommandErrorCode::ProtocolRejected,
            detail: None,
        }),
    }
}
pub(super) fn uncertain() -> CommandDispatchOutcome {
    CommandDispatchOutcome::TransmissionUncertain { detail: "OCPP 1.6 command has no valid authoritative response; observe state before further action".to_owned() }
}
