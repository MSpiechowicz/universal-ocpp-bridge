//! Sensitive, typed charging identity input. Concrete OCPP models remain in adapters.
use crate::{AuthorizationProviderDescriptor, AuthorizationProviderError, AuthorizationReference};
use std::{fmt, future::Future, pin::Pin};

/// Token namespaces are distinct even when their presented bytes are identical.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChargingTokenKind {
    Central,
    Emaid,
    Iso14443,
    Iso15693,
    KeyCode,
    Local,
    MacAddress,
    NoAuthorization,
}

/// Additional identity evidence; never include these fields in diagnostics.
#[derive(Clone, Eq, PartialEq)]
pub struct AdditionalChargingIdentity {
    pub token: String,
    pub kind: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CertificateHashAlgorithm {
    Sha256,
    Sha384,
    Sha512,
}

/// OCSP evidence is data for a trusted provider, not permission to contact its URL.
#[derive(Clone, Eq, PartialEq)]
pub struct ChargingCertificateHash {
    pub algorithm: CertificateHashAlgorithm,
    pub issuer_name_hash: String,
    pub issuer_key_hash: String,
    pub serial_number: String,
    pub responder_url: String,
}

/// Bounded input to a provider. Debug deliberately omits all presented identity material.
#[derive(Clone, Eq, PartialEq)]
pub struct PresentedChargingIdentity {
    pub token: String,
    pub kind: ChargingTokenKind,
    pub additional: Vec<AdditionalChargingIdentity>,
    pub certificate: Option<String>,
    pub certificate_hashes: Vec<ChargingCertificateHash>,
}
impl fmt::Debug for PresentedChargingIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PresentedChargingIdentity([REDACTED])")
    }
}

/// Certificate verification result, independently reported from token authorization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChargingCertificateStatus {
    Accepted,
    SignatureError,
    Expired,
    Missing,
    ChainError,
    Revoked,
    ContractCancelled,
}

/// Provider business denials retain their version-specific response meaning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChargingIdentityDenial {
    Blocked,
    ConcurrentTransaction,
    Expired,
    Invalid,
    NoCredit,
    EvseTypeDenied,
    LocationDenied,
    TimeDenied,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChargingIdentityResolution {
    /// Resolution is not permission to charge: the durable local policy must also allow it.
    Resolved {
        reference: AuthorizationReference,
        certificate: Option<ChargingCertificateStatus>,
    },
    Denied {
        reason: ChargingIdentityDenial,
        certificate: Option<ChargingCertificateStatus>,
    },
}

pub type ChargingIdentityFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<ChargingIdentityResolution, AuthorizationProviderError>>
            + Send
            + 'a,
    >,
>;

/// Provider configured by the composition root; test-only descriptors obey normal runtime gates.
pub trait ChargingIdentityProvider: Send + Sync {
    fn descriptor(&self) -> AuthorizationProviderDescriptor;
    /// Validate the typed evidence without retaining it, logging it, or trusting responder URLs.
    fn resolve<'a>(&'a self, identity: &'a PresentedChargingIdentity)
    -> ChargingIdentityFuture<'a>;
}
