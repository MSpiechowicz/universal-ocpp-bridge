//! Authorization completes only after typed provider resolution and current durable local policy.
use crate::{DecodedCall, OcppCallError, OcppErrorCode};
use serde_json::{Value, json};
use std::time::Duration;
use uob_application::charging_identity::{
    ChargingCertificateStatus, ChargingIdentityDenial, ChargingIdentityProvider,
    ChargingIdentityResolution,
};
use uob_application::{
    AuthorizationDecision, AuthorizationDenialReason, ChargerObservation, CommandClock,
    LocalAuthorizationService,
};
use uob_contracts::ResourceRef;

/// Decode and complete an authorization call for the authenticated station resource.
/// # Errors
/// Rejects invalid/unsupported requests and invalid provider timeout configuration.
pub async fn authorize_call<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    frame: &[u8],
    resource: &ResourceRef,
    authorization: &LocalAuthorizationService<C, E, D, R>,
    provider: &dyn ChargingIdentityProvider,
    clock: &dyn CommandClock,
    timeout: Duration,
) -> Result<Value, OcppCallError> {
    let call = super::decode_call(frame).map_err(|e| e.call_error())?;
    complete_authorization(call, resource, authorization, provider, clock, timeout).await
}

/// Completes an already validated incoming call; send element 2 through its bounded responder.
/// No network or browser/payment state substitutes for local charging permission.
/// # Errors
/// Rejects non-authorization calls and invalid timeout configuration.
pub async fn complete_authorization<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    call: DecodedCall,
    resource: &ResourceRef,
    authorization: &LocalAuthorizationService<C, E, D, R>,
    provider: &dyn ChargingIdentityProvider,
    clock: &dyn CommandClock,
    timeout: Duration,
) -> Result<Value, OcppCallError> {
    let ChargerObservation::ChargingIdentity(identity) = call.observation else {
        return Err(error(
            OcppErrorCode::NotImplemented,
            "Not an authorization request",
        ));
    };
    if timeout.is_zero() || timeout > Duration::from_secs(30) {
        return Err(error(
            OcppErrorCode::InternalError,
            "Invalid authorization timeout",
        ));
    }
    let certificate_required =
        identity.certificate.is_some() || !identity.certificate_hashes.is_empty();
    let resolution = tokio::time::timeout(timeout, provider.resolve(&identity)).await;
    let (status, expiry, certificate) = match resolution {
        Ok(Ok(ChargingIdentityResolution::Resolved {
            reference,
            certificate,
        })) => {
            if certificate.is_some_and(|v| v != ChargingCertificateStatus::Accepted)
                || (certificate_required
                    && certificate != Some(ChargingCertificateStatus::Accepted))
            {
                ("Invalid", None, certificate)
            } else {
                // Read the clock and policy after awaiting the provider: expiry and revocation
                // during a delayed resolution must never earn a stale Accepted response.
                match authorization.authorize_reference(&reference, resource, clock.now()) {
                    AuthorizationDecision::Allowed { expires_at, .. } => {
                        ("Accepted", expires_at, certificate)
                    }
                    AuthorizationDecision::Denied { reason } => {
                        (local_denial(reason), None, certificate)
                    }
                }
            }
        }
        Ok(Ok(ChargingIdentityResolution::Denied {
            reason,
            certificate,
        })) => (provider_denial(reason), None, certificate),
        _ => ("Invalid", None, None),
    };
    let mut response = json!({"idTokenInfo":{"status":status}});
    if let Some(expiry) = expiry {
        response["idTokenInfo"]["cacheExpiryDateTime"] = json!(expiry);
    }
    if let Some(certificate) = certificate {
        response["certificateStatus"] = json!(certificate_status(certificate));
    }
    Ok(json!([3, call.message_id, response]))
}
const fn local_denial(reason: AuthorizationDenialReason) -> &'static str {
    match reason {
        AuthorizationDenialReason::Unknown => "Unknown",
        AuthorizationDenialReason::Revoked => "Blocked",
        AuthorizationDenialReason::Expired => "Expired",
        AuthorizationDenialReason::ResourceDenied => "NotAtThisLocation",
        AuthorizationDenialReason::ProviderUnavailable => "Invalid",
    }
}
const fn provider_denial(reason: ChargingIdentityDenial) -> &'static str {
    match reason {
        ChargingIdentityDenial::Blocked => "Blocked",
        ChargingIdentityDenial::ConcurrentTransaction => "ConcurrentTx",
        ChargingIdentityDenial::Expired => "Expired",
        ChargingIdentityDenial::Invalid => "Invalid",
        ChargingIdentityDenial::NoCredit => "NoCredit",
        ChargingIdentityDenial::EvseTypeDenied => "NotAllowedTypeEVSE",
        ChargingIdentityDenial::LocationDenied => "NotAtThisLocation",
        ChargingIdentityDenial::TimeDenied => "NotAtThisTime",
        ChargingIdentityDenial::Unknown => "Unknown",
    }
}
const fn certificate_status(status: ChargingCertificateStatus) -> &'static str {
    match status {
        ChargingCertificateStatus::Accepted => "Accepted",
        ChargingCertificateStatus::SignatureError => "SignatureError",
        ChargingCertificateStatus::Expired => "CertificateExpired",
        ChargingCertificateStatus::Missing => "NoCertificateAvailable",
        ChargingCertificateStatus::ChainError => "CertChainError",
        ChargingCertificateStatus::Revoked => "CertificateRevoked",
        ChargingCertificateStatus::ContractCancelled => "ContractCancelled",
    }
}
fn error(code: OcppErrorCode, description: &'static str) -> OcppCallError {
    OcppCallError {
        protocol: super::PROTOCOL,
        code,
        description,
        field_path: None,
    }
}
