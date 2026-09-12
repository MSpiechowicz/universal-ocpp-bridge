use super::RemoteStartIdentity;
use rust_ocpp::v1_6::messages::{
    remote_start_transaction::{RemoteStartTransactionRequest, RemoteStartTransactionResponse},
    remote_stop_transaction::{RemoteStopTransactionRequest, RemoteStopTransactionResponse},
    reset::{ResetRequest, ResetResponse},
    unlock_connector::{UnlockConnectorRequest, UnlockConnectorResponse},
};
use serde_json::Value;
use uob_application::CommandDispatchOutcome;
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
    now: UtcTimestamp,
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
    uob_application::registration::accepted(snapshot).map_err(|_| PolicyRejected)?;
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
    let Some(NativeProtocolReference::Ocpp16 {
        connector_id: native,
    }) = command.resource.native_protocol_reference
    else {
        return Err(InvalidParameters);
    };
    match &command.operation {
        CommandOperation::Start {
            authorization_reference,
        } => {
            if !available(snapshot, &command.resource) {
                return Err(PolicyRejected);
            }
            let reference = authorization_reference.as_deref().ok_or(PolicyRejected)?;
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
        CommandOperation::Stop { transaction_id } => {
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
        CommandOperation::Ocpp(operation) if operation.protocol == ProtocolEdition::Ocpp16j => {
            privileged(operation, &command.resource, &snapshot.station, native)
        }
        _ => Err(UnsupportedOperation),
    }
}
fn privileged(
    operation: &uob_contracts::PrivilegedOcppOperation<Value>,
    resource: &ResourceRef,
    station: &ResourceRef,
    native: u32,
) -> Result<(&'static str, Value), CommandErrorCode> {
    use CommandErrorCode::{InvalidParameters, UnsupportedOperation};
    match operation.action.as_str() {
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
        "Reset" => serde_json::from_value::<ResetResponse>(payload.clone()).is_ok(),
        "UnlockConnector" => {
            serde_json::from_value::<UnlockConnectorResponse>(payload.clone()).is_ok()
        }
        _ => false,
    };
    if !valid {
        return uncertain();
    }
    if payload["status"] == "Accepted"
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
