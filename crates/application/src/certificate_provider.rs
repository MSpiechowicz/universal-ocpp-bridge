//! Application-owned PKI port: station CSR signing, installable roots and trust decisions.
//!
//! Only public certificate material crosses this port. Station private keys never leave the
//! station, and provider keys never leave the provider; PEM values that carry a private key
//! block are rejected at construction.

use std::{error::Error, fmt, future::Future, pin::Pin};

use uob_contracts::UtcTimestamp;

/// Largest single PEM certificate or CSR, matching the OCPP 2.0.1 certificate bound.
pub const MAX_CERTIFICATE_PEM_BYTES: usize = 5_500;
/// Largest PEM certificate chain, matching the OCPP 2.0.1 `certificateChain` bound.
pub const MAX_CERTIFICATE_CHAIN_PEM_BYTES: usize = 10_000;
/// Most certificates accepted in one chain.
pub const MAX_CERTIFICATE_CHAIN_LENGTH: usize = 5;
/// Most installable roots returned for one kind.
pub const MAX_TRUST_ANCHORS: usize = 4;
const MAX_COMMON_NAME_BYTES: usize = 64;

/// Object-safe future returned by certificate providers.
pub type CertificateProviderFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, CertificateProviderError>> + Send + 'a>>;

/// Describes a certificate provider without exposing configuration or keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CertificateProviderDescriptor {
    /// Stable implementation kind.
    pub kind: &'static str,
    /// Whether this provider is restricted to staging and demo environments.
    pub test_only: bool,
}

macro_rules! pem_text {
    ($name:ident, $label:literal, $maximum:expr, $blocks:expr, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub struct $name(String);

        impl $name {
            /// Validates bounded ASCII PEM holding only the expected block type.
            ///
            /// # Errors
            ///
            /// Returns [`CertificateProviderError::InvalidRequest`] for oversized, malformed,
            /// mixed or private-key-bearing PEM text.
            pub fn new(value: impl Into<String>) -> Result<Self, CertificateProviderError> {
                let value = value.into();
                let blocks = pem_blocks(&value, $label, $maximum)
                    .ok_or(CertificateProviderError::InvalidRequest)?;
                if !$blocks.contains(&blocks) {
                    return Err(CertificateProviderError::InvalidRequest);
                }
                Ok(Self(value))
            }

            /// Returns the PEM text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

pem_text!(
    CertificatePem,
    "CERTIFICATE",
    MAX_CERTIFICATE_PEM_BYTES,
    1..=1,
    "One PEM-encoded X.509 certificate."
);
pem_text!(
    CertificateChainPem,
    "CERTIFICATE",
    MAX_CERTIFICATE_CHAIN_PEM_BYTES,
    1..=MAX_CERTIFICATE_CHAIN_LENGTH,
    "Leaf-first PEM certificate chain."
);
pem_text!(
    CsrPem,
    "CERTIFICATE REQUEST",
    MAX_CERTIFICATE_PEM_BYTES,
    1..=1,
    "One PEM-encoded PKCS #10 certificate signing request."
);

fn pem_blocks(value: &str, label: &str, maximum: usize) -> Option<usize> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let trimmed = value.trim();
    let blocks = value.matches(&begin).count();
    let valid = value.len() <= maximum
        && value.is_ascii()
        && !value.contains("PRIVATE KEY")
        && trimmed.starts_with(&begin)
        && trimmed.ends_with(&end)
        && blocks == value.matches(&end).count()
        && blocks == value.matches("-----BEGIN ").count();
    valid.then_some(blocks)
}

/// Expected certificate common name, such as a station's unique serial number.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CommonName(String);

impl CommonName {
    /// Validates 1 to 64 printable ASCII characters.
    ///
    /// # Errors
    ///
    /// Returns [`CertificateProviderError::InvalidRequest`] for an invalid value.
    pub fn new(value: impl Into<String>) -> Result<Self, CertificateProviderError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_COMMON_NAME_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(CertificateProviderError::InvalidRequest);
        }
        Ok(Self(value))
    }

    /// Returns the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Certificate a station asks the CSMS to have signed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CertificateUse {
    /// Client certificate for the station's CSMS connection.
    ChargingStation,
    /// ISO 15118 SECC certificate.
    V2g,
}

/// Root certificate types the CSMS can install on a station.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TrustAnchorKind {
    /// Root the station uses to authenticate the CSMS.
    CsmsRoot,
    /// Root the station uses to verify firmware signing certificates.
    ManufacturerRoot,
    /// ISO 15118 V2G root.
    V2gRoot,
    /// ISO 15118 mobility-operator root.
    MoRoot,
}

/// What a verified chain must be trusted for.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChainPurpose {
    /// Station client authentication towards the CSMS.
    ChargingStation,
    /// Firmware signing under the manufacturer root.
    FirmwareSigning,
    /// ISO 15118 SECC certificate under the V2G root.
    V2g,
}

/// Station CSR received through an OCPP sign-certificate request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertificateSigningRequest {
    /// Common name the signed certificate must carry, from the station's trusted identity.
    pub expected_common_name: CommonName,
    /// Requested certificate type.
    pub certificate_use: CertificateUse,
    /// Untrusted CSR text.
    pub csr: CsrPem,
}

/// Signed station certificate with any issuing CA certificates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedCertificate {
    /// Leaf-first chain to send to the station.
    pub chain: CertificateChainPem,
    /// Whether this is demo-only material that production must refuse.
    pub test_only: bool,
}

/// Provider decision on one CSR.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CsrDecision {
    /// The CSR was accepted and signed.
    Signed(SignedCertificate),
    /// The CSR was refused for a sanitized reason.
    Rejected(CsrRejection),
}

/// Sanitized reason a CSR was not signed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CsrRejection {
    /// The CSR could not be decoded.
    Malformed,
    /// The CSR's self-signature does not verify.
    InvalidSignature,
    /// The subject does not name the expected station or organization.
    SubjectMismatch,
    /// The key algorithm or size is not accepted.
    UnsupportedKey,
    /// The CSR requests extensions the provider does not sign.
    UnsupportedRequest,
    /// Provider policy refused the request.
    PolicyRejected,
}

/// Installable roots of one kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustAnchors {
    certificates: Vec<CertificatePem>,
    test_only: bool,
}

impl TrustAnchors {
    /// Bounds the roots returned for one kind.
    ///
    /// # Errors
    ///
    /// Returns [`CertificateProviderError::InvalidProviderResponse`] for more than
    /// [`MAX_TRUST_ANCHORS`] certificates.
    pub fn new(
        certificates: Vec<CertificatePem>,
        test_only: bool,
    ) -> Result<Self, CertificateProviderError> {
        if certificates.len() > MAX_TRUST_ANCHORS {
            return Err(CertificateProviderError::InvalidProviderResponse);
        }
        Ok(Self {
            certificates,
            test_only,
        })
    }

    /// Root certificates; empty when the provider has none of this kind.
    #[must_use]
    pub fn certificates(&self) -> &[CertificatePem] {
        &self.certificates
    }

    /// Whether these are demo-only roots that production must refuse.
    #[must_use]
    pub const fn test_only(&self) -> bool {
        self.test_only
    }
}

/// Request for a trust decision on an untrusted chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainVerification {
    /// Required use of the leaf certificate.
    pub purpose: ChainPurpose,
    /// Untrusted leaf-first chain; the root may be omitted.
    pub chain: CertificateChainPem,
    /// Common name the leaf must carry, when the caller knows it.
    pub expected_common_name: Option<CommonName>,
    /// Instant at which validity periods are checked.
    pub at: UtcTimestamp,
}

/// Provider trust decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustDecision {
    /// The chain reaches a configured root for the purpose.
    Trusted,
    /// The chain is not trusted for a sanitized reason.
    Untrusted(TrustFailure),
}

/// Sanitized reason a chain is not trusted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustFailure {
    /// A certificate could not be decoded.
    Malformed,
    /// No configured root issued the chain.
    UnknownIssuer,
    /// A certificate expired before the checked instant.
    Expired,
    /// A certificate is not yet valid at the checked instant.
    NotYetValid,
    /// The leaf is not permitted for the requested purpose.
    WrongPurpose,
    /// The leaf subject does not name the expected station or organization.
    SubjectMismatch,
    /// A signature in the chain does not verify.
    InvalidSignature,
    /// The chain is longer than the provider accepts.
    ChainTooLong,
}

/// Sanitized provider failures without key material, certificates or paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CertificateProviderError {
    /// The request was structurally invalid for this port.
    InvalidRequest,
    /// The provider cannot currently serve requests.
    Unavailable,
    /// The provider did not answer before its deadline.
    TimedOut,
    /// The provider returned a value that violates this port.
    InvalidProviderResponse,
}

impl fmt::Display for CertificateProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRequest => "certificate request is invalid",
            Self::Unavailable => "certificate provider is unavailable",
            Self::TimedOut => "certificate provider timed out",
            Self::InvalidProviderResponse => "certificate provider response is invalid",
        })
    }
}

impl Error for CertificateProviderError {}

/// Provider boundary for station certificate signing and trust decisions.
pub trait CertificateProvider: Send + Sync {
    /// Describes the provider without exposing configuration or keys.
    fn descriptor(&self) -> CertificateProviderDescriptor;

    /// Verifies and signs a station CSR, or refuses it for a sanitized reason.
    fn sign_csr<'a>(
        &'a self,
        request: &'a CertificateSigningRequest,
    ) -> CertificateProviderFuture<'a, CsrDecision>;

    /// Returns the roots of one kind that the CSMS can install.
    fn trust_anchors(&self, kind: TrustAnchorKind) -> CertificateProviderFuture<'_, TrustAnchors>;

    /// Decides whether an untrusted chain reaches a configured root for its purpose.
    fn verify_chain<'a>(
        &'a self,
        request: &'a ChainVerification,
    ) -> CertificateProviderFuture<'a, TrustDecision>;
}

#[cfg(test)]
mod tests;
