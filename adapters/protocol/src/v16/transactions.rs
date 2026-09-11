use super::PROTOCOL;
use crate::{DecodedCall, OcppCallError, OcppErrorCode};
use serde_json::{Value, json};
use std::time::Duration;
use uob_application::{
    AuthorizationDecision, AuthorizationDenialReason, AuthorizationProvider, ChargerObservation,
    CommandClock, LocalAuthorizationService, OperationalStore, SensitiveAuthorizationToken,
    transaction16::{self, TransactionContext, TransactionError},
};
use uob_contracts::{StationSnapshot, TransactionSnapshot};

/// All calls share the authoritative store, current policy, and trusted clock.
/// The station owner must serialize calls and restore its snapshot before reconnect handling.
pub struct TransactionServices<'a, C, R> {
    pub store: &'a dyn OperationalStore<C, TransactionSnapshot, TransactionSnapshot, R>,
    pub authorization:
        &'a LocalAuthorizationService<C, TransactionSnapshot, TransactionSnapshot, R>,
    pub provider: &'a dyn AuthorizationProvider,
    pub clock: &'a dyn CommandClock,
    pub authorization_timeout: Duration,
}

/// Decodes a bounded transaction call and commits before exposing a response.
/// # Errors
/// Rejects invalid input, conflicts, resource pressure or storage failure with safe CALLERRORs.
pub async fn transaction_call<C: Send + 'static, R: Send + 'static>(
    frame: &[u8],
    snapshot: &mut StationSnapshot,
    services: &TransactionServices<'_, C, R>,
    context: TransactionContext,
) -> Result<Value, OcppCallError> {
    if frame.len() > 256 * 1024 {
        return Err(error(OcppErrorCode::PropertyConstraintViolation));
    }
    let call = super::decode_call(frame).map_err(|e| e.call_error())?;
    complete_transaction(call, snapshot, services, context).await
}

/// Completes a validated call from the authenticated bounded OCPP session.
/// Send element 2 with the incoming responder; never respond before this future completes.
/// # Errors
/// A failed commit leaves the authoritative in-memory snapshot unchanged.
pub async fn complete_transaction<C: Send + 'static, R: Send + 'static>(
    call: DecodedCall,
    snapshot: &mut StationSnapshot,
    services: &TransactionServices<'_, C, R>,
    context: TransactionContext,
) -> Result<Value, OcppCallError> {
    let (transaction, duplicate, response) = match call.observation {
        ChargerObservation::TransactionStarted(observation) if observation.protocol == PROTOCOL => {
            if let Some(transaction) = transaction16::replay_start(
                snapshot,
                &observation,
                &call.message_id,
                services.clock.now(),
            )
            .map_err(|e| lifecycle(&e))?
            {
                let response = start_response(&transaction)?;
                return Ok(json!([3, call.message_id, response]));
            }
            if services.authorization_timeout.is_zero()
                || services.authorization_timeout > Duration::from_secs(30)
            {
                return Err(error(OcppErrorCode::InternalError));
            }
            let resource = snapshot
                .resources
                .iter()
                .find(|r| r.resource.native_protocol_reference == Some(observation.native_resource))
                .ok_or_else(|| error(OcppErrorCode::ProtocolError))?;
            let token = SensitiveAuthorizationToken::new(&observation.identity.token)
                .map_err(|_| error(OcppErrorCode::PropertyConstraintViolation))?;
            // Consult policy only after resolution, so delayed revocation/expiry remains effective.
            let (decision, reference) = match tokio::time::timeout(
                services.authorization_timeout,
                services.provider.resolve(&token),
            )
            .await
            {
                Ok(Ok(reference)) => (
                    services.authorization.authorize_reference(
                        &reference,
                        &resource.resource,
                        services.clock.now(),
                    ),
                    Some(reference.as_str().to_owned()),
                ),
                _ => (
                    AuthorizationDecision::Denied {
                        reason: AuthorizationDenialReason::ProviderUnavailable,
                    },
                    None,
                ),
            };
            let (status, expiry) = match decision {
                AuthorizationDecision::Allowed { expires_at, .. } => ("Accepted", expires_at),
                AuthorizationDecision::Denied { reason } => (
                    match reason {
                        AuthorizationDenialReason::Revoked => "Blocked",
                        AuthorizationDenialReason::Expired => "Expired",
                        _ => "Invalid",
                    },
                    None,
                ),
            };
            let id = services
                .store
                .reserve_transaction_id()
                .await
                .map_err(|_| error(OcppErrorCode::InternalError))?;
            let transaction = transaction16::start(
                snapshot,
                &observation,
                call.message_id.clone(),
                id,
                status.to_owned(),
                expiry,
                reference,
                services.clock.now(),
            )
            .map_err(|e| lifecycle(&e))?;
            let response = start_response(&transaction)?;
            (transaction, false, response)
        }
        ChargerObservation::TransactionStopped(observation) => {
            let (transaction, duplicate) =
                transaction16::stop(snapshot, &observation, &call.message_id)
                    .map_err(|e| lifecycle(&e))?;
            (transaction, duplicate, json!({}))
        }
        _ => return Err(error(OcppErrorCode::NotImplemented)),
    };
    if !duplicate {
        transaction16::commit(
            services.store,
            snapshot,
            transaction,
            context,
            services.clock.now(),
        )
        .await
        .map_err(|e| lifecycle(&e))?;
    }
    Ok(json!([3, call.message_id, response]))
}
fn start_response(transaction: &TransactionSnapshot) -> Result<Value, OcppCallError> {
    let state = transaction
        .ocpp16
        .as_ref()
        .ok_or_else(|| error(OcppErrorCode::InternalError))?;
    let mut info = json!({"status": state.authorization_status});
    if let Some(expiry) = state.authorization_expiry {
        info["expiryDate"] = json!(expiry);
    }
    Ok(json!({"transactionId": state.transaction_id, "idTagInfo": info}))
}
fn lifecycle(value: &TransactionError) -> OcppCallError {
    error(match value {
        TransactionError::Storage(_) | TransactionError::Capacity => OcppErrorCode::InternalError,
        TransactionError::InvalidState | TransactionError::Conflict => OcppErrorCode::ProtocolError,
    })
}
fn error(code: OcppErrorCode) -> OcppCallError {
    OcppCallError {
        protocol: PROTOCOL,
        code,
        description: "Transaction lifecycle request could not be completed",
        field_path: None,
    }
}
