use rust_ocpp::v1_6::{
    messages::{
        authorize::{AuthorizeRequest, AuthorizeResponse},
        start_transaction::StartTransactionRequest,
    },
    types::{AuthorizationStatus, IdTagInfo},
};
use serde_json::{Value, json};
use std::time::Duration;
use uob_application::{
    AuthorizationDecision, AuthorizationDenialReason, AuthorizationProvider,
    LocalAuthorizationService, SensitiveAuthorizationToken,
};
use uob_application::{
    ChargerObservation, CommandClock,
    charging_identity::{ChargingTokenKind, PresentedChargingIdentity},
};
use uob_contracts::{NativeProtocolReference, ResourceRef, UtcTimestamp};

use super::{PROTOCOL, parse_frame, validated_payload};
use crate::{DecodeError, DecodeErrorKind, DecodedCall, OcppCallError, OcppErrorCode};

/// OCPP 1.6 workflow whose presented `idTag` was checked by application policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ocpp16AuthorizationFlow {
    Authorize,
    TransactionStart,
}

/// Correlated local-policy result for one OCPP 1.6 authorization-bearing call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ocpp16AuthorizationOutcome {
    pub message_id: String,
    pub flow: Ocpp16AuthorizationFlow,
    pub decision: AuthorizationDecision,
}

impl Ocpp16AuthorizationOutcome {
    /// Builds the OCPP `Authorize.conf` frame while retaining the request message ID.
    ///
    /// Transaction starts are completed by the transaction lifecycle because their response also
    /// requires a CSMS-assigned transaction ID.
    ///
    /// # Panics
    ///
    /// Panics only if the canonical UTC timestamp can no longer round-trip through its RFC 3339
    /// serialization into the pinned OCPP model's UTC timestamp type.
    #[must_use]
    pub fn authorize_response_frame(&self) -> Option<Value> {
        if self.flow != Ocpp16AuthorizationFlow::Authorize {
            return None;
        }
        let (status, expiry_date) = match &self.decision {
            AuthorizationDecision::Allowed { expires_at, .. } => (AuthorizationStatus::Accepted, {
                expires_at.map(|value| {
                    let encoded =
                        serde_json::to_value(value).expect("UTC timestamp always serializes");
                    serde_json::from_value(encoded)
                        .expect("UTC timestamp is a chrono-compatible RFC 3339 value")
                })
            }),
            AuthorizationDecision::Denied { reason } => (denial_status(*reason), None),
        };
        let response = AuthorizeResponse {
            id_tag_info: IdTagInfo {
                expiry_date,
                parent_id_tag: None,
                status,
            },
        };
        Some(json!([3, self.message_id, response]))
    }
}

/// Validates and authorizes an OCPP 1.6 `Authorize` or `StartTransaction` call.
///
/// Every call resolves the sensitive token and consults current durable application policy. No
/// positive protocol-adapter cache can therefore outlive a local revocation. For transaction
/// starts, the host must supply the canonical resource whose native connector matches the call.
///
/// # Errors
///
/// Returns [`DecodeError`] when the CALL envelope, action, typed payload, identifier, or trusted
/// native connector association is invalid.
pub async fn authorize_call<C, E, D, R>(
    frame: &[u8],
    resource: &ResourceRef,
    authorization: &LocalAuthorizationService<C, E, D, R>,
    provider: &dyn AuthorizationProvider,
    now: UtcTimestamp,
) -> Result<Ocpp16AuthorizationOutcome, DecodeError>
where
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    let (message_id, action, payload) = parse_frame(frame)?;
    let (flow, id_tag) = match action.as_str() {
        "Authorize" => {
            let request: AuthorizeRequest = validated_payload(payload)?;
            (Ocpp16AuthorizationFlow::Authorize, request.id_tag)
        }
        "StartTransaction" => {
            let request: StartTransactionRequest = validated_payload(payload)?;
            require_connector(resource, request.connector_id)?;
            (Ocpp16AuthorizationFlow::TransactionStart, request.id_tag)
        }
        _ => {
            return Err(DecodeError::new(
                PROTOCOL,
                DecodeErrorKind::UnsupportedAction,
            ));
        }
    };
    let token = SensitiveAuthorizationToken::new(id_tag)
        .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?;
    let decision = authorization
        .authorize_token(provider, &token, resource, now)
        .await;
    Ok(Ocpp16AuthorizationOutcome {
        message_id,
        flow,
        decision,
    })
}

fn require_connector(resource: &ResourceRef, connector_id: u32) -> Result<(), DecodeError> {
    if resource.native_protocol_reference == Some(NativeProtocolReference::Ocpp16 { connector_id })
    {
        Ok(())
    } else {
        Err(DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))
    }
}

const fn denial_status(reason: AuthorizationDenialReason) -> AuthorizationStatus {
    match reason {
        AuthorizationDenialReason::Revoked => AuthorizationStatus::Blocked,
        AuthorizationDenialReason::Expired => AuthorizationStatus::Expired,
        AuthorizationDenialReason::Unknown
        | AuthorizationDenialReason::ResourceDenied
        | AuthorizationDenialReason::ProviderUnavailable => AuthorizationStatus::Invalid,
    }
}

pub(super) fn observation(payload: Value) -> Result<ChargerObservation, DecodeError> {
    if !payload
        .as_object()
        .is_some_and(|fields| fields.len() == 1 && fields.contains_key("idTag"))
    {
        return Err(DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload));
    }
    let request: AuthorizeRequest = validated_payload(payload)?;
    if request.id_tag.is_empty() {
        return Err(DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload));
    }
    Ok(ChargerObservation::ChargingIdentity(
        PresentedChargingIdentity {
            token: request.id_tag,
            kind: ChargingTokenKind::Local,
            additional: Vec::new(),
            certificate: None,
            certificate_hashes: Vec::new(),
        },
    ))
}

/// Completes a validated native Authorize call using the current durable service policy.
/// # Errors
/// Rejects non-native authorization input and invalid provider timeout configuration.
/// # Panics
/// Panics only if a canonical policy timestamp cannot round-trip through the pinned native model.
pub async fn complete_authorization<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    call: DecodedCall,
    resource: &ResourceRef,
    authorization: &LocalAuthorizationService<C, E, D, R>,
    provider: &dyn AuthorizationProvider,
    clock: &dyn CommandClock,
    timeout: Duration,
) -> Result<Value, OcppCallError> {
    let error = |code| OcppCallError {
        protocol: PROTOCOL,
        code,
        description: "Native authorization unavailable",
        field_path: None,
    };
    if call.action.as_str() != "Authorize" {
        return Err(error(OcppErrorCode::NotImplemented));
    }
    let ChargerObservation::ChargingIdentity(identity) = call.observation else {
        return Err(error(OcppErrorCode::NotImplemented));
    };
    if identity.kind != ChargingTokenKind::Local
        || !identity.additional.is_empty()
        || identity.certificate.is_some()
        || !identity.certificate_hashes.is_empty()
    {
        return Err(error(OcppErrorCode::NotImplemented));
    }
    if timeout.is_zero() || timeout > Duration::from_secs(30) {
        return Err(error(OcppErrorCode::InternalError));
    }
    let token = SensitiveAuthorizationToken::new(&identity.token)
        .map_err(|_| error(OcppErrorCode::PropertyConstraintViolation))?;
    let decision = match tokio::time::timeout(timeout, provider.resolve(&token)).await {
        Ok(Ok(reference)) => authorization.authorize_reference(&reference, resource, clock.now()),
        _ => AuthorizationDecision::Denied {
            reason: AuthorizationDenialReason::ProviderUnavailable,
        },
    };
    Ok(Ocpp16AuthorizationOutcome {
        message_id: call.message_id,
        flow: Ocpp16AuthorizationFlow::Authorize,
        decision,
    }
    .authorize_response_frame()
    .expect("native Authorize outcome"))
}
