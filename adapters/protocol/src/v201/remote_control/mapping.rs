use super::RemoteStartIdentity;
use rust_ocpp::v2_0_1::messages::{
    request_start_transaction::{RequestStartTransactionRequest, RequestStartTransactionResponse},
    request_stop_transaction::{RequestStopTransactionRequest, RequestStopTransactionResponse},
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
    remote_start_id: Option<i32>,
) -> Result<(&'static str, Value), CommandErrorCode> {
    use CommandErrorCode::{InvalidParameters, PolicyRejected, UnsupportedOperation};
    if !matches!(
        snapshot.connectivity,
        Connectivity::Connected {
            protocol: ProtocolEdition::Ocpp201,
            ..
        }
    ) {
        return Err(CommandErrorCode::StationDisconnected);
    }
    uob_application::registration::v201::accepted(snapshot).map_err(|_| PolicyRejected)?;
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
    let native = if command.resource == snapshot.station {
        None
    } else {
        Some(
            command
                .resource
                .native_protocol_reference
                .ok_or(InvalidParameters)?,
        )
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
            let evse_id = match native {
                None => None,
                Some(NativeProtocolReference::Ocpp201 {
                    evse_id,
                    connector_id: None,
                }) if evse_id > 0 => Some(i32::try_from(evse_id).map_err(|_| InvalidParameters)?),
                // OCPP start cannot address a connector. Never silently widen its authorization.
                _ => return Err(InvalidParameters),
            };
            let request = RequestStartTransactionRequest {
                evse_id,
                remote_start_id: remote_start_id
                    .filter(|id| *id > 0)
                    .ok_or(InvalidParameters)?,
                id_token: serde_json::from_value(token_value(&token)?)
                    .map_err(|_| InvalidParameters)?,
                charging_profile: None,
                group_id_token: None,
            };
            Ok(("RequestStartTransaction", encode(request)?))
        }
        CommandOperation::Stop { transaction_id } => {
            let tx = snapshot
                .transactions
                .iter()
                .find(|tx| {
                    &tx.transaction_id == transaction_id
                        && covers(&command.resource, &tx.resource)
                        && tx.state != TransactionState::Ended
                })
                .ok_or(InvalidParameters)?;
            let state = tx
                .protocol_state
                .as_ref()
                .filter(|s| s.protocol == ProtocolEdition::Ocpp201)
                .ok_or(InvalidParameters)?;
            let native_id = state.native_transaction_id.clone();
            if native_id.is_empty() || native_id.chars().count() > 36 {
                return Err(InvalidParameters);
            }
            Ok((
                "RequestStopTransaction",
                encode(RequestStopTransactionRequest {
                    transaction_id: native_id,
                })?,
            ))
        }
        CommandOperation::Ocpp(operation) if operation.protocol == ProtocolEdition::Ocpp201 => {
            privileged(operation, &command.resource, &snapshot.station, native)
        }
        _ => Err(UnsupportedOperation),
    }
}
fn privileged(
    operation: &uob_contracts::PrivilegedOcppOperation<Value>,
    resource: &ResourceRef,
    station: &ResourceRef,
    native: Option<NativeProtocolReference>,
) -> Result<(&'static str, Value), CommandErrorCode> {
    use CommandErrorCode::{InvalidParameters, UnsupportedOperation};
    match operation.action.as_str() {
        "Reset" if operation.payload_schema.as_str() == "urn:OCPP:Cp:2:2020:3:ResetRequest" => {
            fields(&operation.payload, &["type", "evseId"])?;
            let request: ResetRequest =
                serde_json::from_value(operation.payload.clone()).map_err(|_| InvalidParameters)?;
            let expected = match native {
                None if resource == station => None,
                Some(NativeProtocolReference::Ocpp201 {
                    evse_id,
                    connector_id: None,
                }) if evse_id > 0 => Some(i32::try_from(evse_id).map_err(|_| InvalidParameters)?),
                _ => return Err(InvalidParameters),
            };
            if request.evse_id != expected {
                return Err(InvalidParameters);
            }
            Ok(("Reset", encode(request)?))
        }
        "UnlockConnector"
            if operation.payload_schema.as_str()
                == "urn:OCPP:Cp:2:2020:3:UnlockConnectorRequest" =>
        {
            exact_fields(&operation.payload, &["evseId", "connectorId"])?;
            let request: UnlockConnectorRequest =
                serde_json::from_value(operation.payload.clone()).map_err(|_| InvalidParameters)?;
            if !matches!(native, Some(NativeProtocolReference::Ocpp201 { evse_id, connector_id: Some(connector_id) })
                if request.evse_id > 0 && request.connector_id > 0 && u32::try_from(request.evse_id).ok() == Some(evse_id) && u32::try_from(request.connector_id).ok() == Some(connector_id))
            {
                return Err(InvalidParameters);
            }
            Ok(("UnlockConnector", encode(request)?))
        }
        _ => Err(UnsupportedOperation),
    }
}
pub(super) fn covers(scope: &ResourceRef, resource: &ResourceRef) -> bool {
    use uob_contracts::CanonicalResource;
    if scope.bridge_id != resource.bridge_id || scope.station_id != resource.station_id {
        return false;
    }
    if scope.resource.is_none() || scope == resource {
        return true;
    }
    matches!((&scope.resource, &resource.resource),
        (Some(CanonicalResource::Evse { evse_id: a, connector_id: None }), Some(CanonicalResource::Evse { evse_id: b, .. })) if a == b)
        && matches!((scope.native_protocol_reference, resource.native_protocol_reference),
            (Some(NativeProtocolReference::Ocpp201 { evse_id: a, connector_id: None }), Some(NativeProtocolReference::Ocpp201 { evse_id: b, .. })) if a == b && a > 0)
}
fn available(snapshot: &StationSnapshot, resource: &ResourceRef) -> bool {
    if resource != &snapshot.station
        && !snapshot.resources.iter().any(|entry| {
            &entry.resource == resource && entry.availability == AvailabilityState::Available
        })
    {
        return false;
    }
    snapshot.resources.iter().any(|entry| {
        let Some(NativeProtocolReference::Ocpp201 { evse_id, .. }) = entry.resource.native_protocol_reference else { return false; };
        covers(resource, &entry.resource) && entry.availability == AvailabilityState::Available
            && !snapshot.transactions.iter().any(|tx| tx.state != TransactionState::Ended && matches!(tx.resource.native_protocol_reference, Some(NativeProtocolReference::Ocpp201 { evse_id: active, .. }) if active == evse_id))
    })
}
fn fields(value: &Value, names: &[&str]) -> Result<(), CommandErrorCode> {
    if value
        .as_object()
        .is_some_and(|o| o.keys().all(|k| names.contains(&k.as_str())))
    {
        Ok(())
    } else {
        Err(CommandErrorCode::InvalidParameters)
    }
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
    let allowed: &[&str] = if action == "RequestStartTransaction" {
        &["status", "statusInfo", "transactionId"]
    } else {
        &["status", "statusInfo"]
    };
    if fields(payload, allowed).is_err() || !valid_details(payload) {
        return uncertain();
    }
    // Decode with the action-specific pinned model; unknown statuses are not a denial or success.
    let valid = match action {
        "RequestStartTransaction" => {
            serde_json::from_value::<RequestStartTransactionResponse>(payload.clone()).is_ok()
        }
        "RequestStopTransaction" => {
            serde_json::from_value::<RequestStopTransactionResponse>(payload.clone()).is_ok()
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
        || (action == "Reset" && payload["status"] == "Scheduled")
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
    CommandDispatchOutcome::TransmissionUncertain { detail: "OCPP 2.0.1 command has no valid authoritative response; observe state before further action".to_owned() }
}

fn valid_details(payload: &Value) -> bool {
    if payload.get("transactionId").is_some_and(|id| {
        id.as_str()
            .is_none_or(|id| id.is_empty() || id.chars().count() > 36)
    }) {
        return false;
    }
    payload.get("statusInfo").is_none_or(|info| {
        fields(info, &["reasonCode", "additionalInfo"]).is_ok()
            && info
                .get("reasonCode")
                .and_then(Value::as_str)
                .is_some_and(|s| s.chars().count() <= 20)
            && info
                .get("additionalInfo")
                .is_none_or(|v| v.as_str().is_some_and(|s| s.chars().count() <= 512))
    })
}
pub(super) fn token_value(
    token: &uob_application::charging_identity::PresentedChargingIdentity,
) -> Result<Value, CommandErrorCode> {
    use uob_application::charging_identity::ChargingTokenKind as K;
    let kind = match token.kind {
        K::Central => "Central",
        K::Emaid => "eMAID",
        K::Iso14443 => "ISO14443",
        K::Iso15693 => "ISO15693",
        K::KeyCode => "KeyCode",
        K::Local => "Local",
        K::MacAddress => "MacAddress",
        K::NoAuthorization => "NoAuthorization",
    };
    if token.token.is_empty()
        || token.token.chars().count() > 36
        || !token.additional.is_empty()
        || token.certificate.is_some()
        || !token.certificate_hashes.is_empty()
    {
        return Err(CommandErrorCode::InvalidParameters);
    }
    Ok(serde_json::json!({"idToken":token.token,"type":kind}))
}
