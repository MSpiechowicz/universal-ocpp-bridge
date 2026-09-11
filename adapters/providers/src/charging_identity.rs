use sha2::{Digest, Sha256};
use uob_application::charging_identity::{
    ChargingIdentityDenial, ChargingIdentityFuture, ChargingIdentityProvider,
    ChargingIdentityResolution, ChargingTokenKind, PresentedChargingIdentity,
};
use uob_application::{
    AuthorizationProviderDescriptor, AuthorizationProviderError, AuthorizationReference,
};

/// Offline typed identity resolver. Certificate/additional evidence needs a dedicated provider.
#[derive(Clone, Copy, Default)]
pub struct LocalChargingIdentityProvider;
impl ChargingIdentityProvider for LocalChargingIdentityProvider {
    fn descriptor(&self) -> AuthorizationProviderDescriptor {
        AuthorizationProviderDescriptor {
            kind: "local.typed-sha256",
            test_only: false,
        }
    }
    fn resolve<'a>(
        &'a self,
        identity: &'a PresentedChargingIdentity,
    ) -> ChargingIdentityFuture<'a> {
        Box::pin(async move {
            if identity.certificate.is_some()
                || !identity.certificate_hashes.is_empty()
                || !identity.additional.is_empty()
                || matches!(
                    identity.kind,
                    ChargingTokenKind::Emaid | ChargingTokenKind::NoAuthorization
                )
            {
                return Ok(ChargingIdentityResolution::Denied {
                    reason: ChargingIdentityDenial::Invalid,
                    certificate: None,
                });
            }
            if identity.token.is_empty() || identity.token.chars().count() > 36 {
                return Err(AuthorizationProviderError::InvalidToken);
            }
            // Versioned namespace prevents collisions with raw 1.6 tokens and other token kinds.
            let kind = match identity.kind {
                ChargingTokenKind::Central => "Central",
                ChargingTokenKind::Iso14443 => "ISO14443",
                ChargingTokenKind::Iso15693 => "ISO15693",
                ChargingTokenKind::KeyCode => "KeyCode",
                ChargingTokenKind::Local => "Local",
                ChargingTokenKind::MacAddress => "MacAddress",
                ChargingTokenKind::Emaid | ChargingTokenKind::NoAuthorization => unreachable!(),
            };
            let mut hash = Sha256::new();
            hash.update(b"uob-charging-identity-v1\0");
            hash.update(kind.as_bytes());
            hash.update(b"\0");
            hash.update(identity.token.to_uppercase().as_bytes());
            let mut encoded = String::from("sha256:");
            for byte in hash.finalize() {
                use std::fmt::Write as _;
                write!(encoded, "{byte:02x}").expect("String write");
            }
            Ok(ChargingIdentityResolution::Resolved {
                reference: AuthorizationReference::new(encoded)
                    .map_err(|_| AuthorizationProviderError::InvalidToken)?,
                certificate: None,
            })
        })
    }
}
