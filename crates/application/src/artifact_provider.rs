//! Application-owned artifact provider port for firmware and station log transfers.
//!
//! A provider resolves an operator-chosen reference to a charger-reachable location with
//! integrity metadata, and opens bounded destinations for station log uploads. Values crossing
//! this port never carry credentials, private keys or local filesystem paths.

use std::{error::Error, fmt, future::Future, pin::Pin};

use crate::certificate_provider::CertificatePem;

/// Longest charger-reachable location, matching the OCPP 2.0.1 firmware and log URL bound.
pub const MAX_ARTIFACT_LOCATION_BYTES: usize = 512;
/// Longest base64 firmware signature accepted by OCPP 2.0.1.
pub const MAX_FIRMWARE_SIGNATURE_BYTES: usize = 800;
const MAX_REFERENCE_BYTES: usize = 128;
const MAX_UPLOAD_ID_BYTES: usize = 64;
const LOCATION_SCHEMES: [&str; 4] = ["http://", "https://", "ftp://", "ftps://"];

/// Object-safe future returned by artifact providers.
pub type ArtifactProviderFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ArtifactProviderError>> + Send + 'a>>;

/// Describes an artifact provider without exposing configuration or credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactProviderDescriptor {
    /// Stable implementation kind.
    pub kind: &'static str,
    /// Whether this provider is restricted to staging and demo environments.
    pub test_only: bool,
}

/// What an artifact contains, which fixes the direction it travels.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ArtifactKind {
    /// Unsigned firmware image downloaded by a station.
    Firmware,
    /// Firmware image with a signing certificate and signature.
    SignedFirmware,
    /// Diagnostics log uploaded by a station.
    DiagnosticsLog,
    /// Security log uploaded by a station.
    SecurityLog,
}

impl ArtifactKind {
    /// Whether stations download this kind.
    #[must_use]
    pub const fn is_firmware(self) -> bool {
        matches!(self, Self::Firmware | Self::SignedFirmware)
    }

    /// Whether stations upload this kind.
    #[must_use]
    pub const fn is_log(self) -> bool {
        matches!(self, Self::DiagnosticsLog | Self::SecurityLog)
    }
}

/// Operator-chosen artifact name: ASCII letters, digits, `.`, `-` and `_`, not starting with `.`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactReference(String);

impl ArtifactReference {
    /// Validates a bounded reference that cannot name a parent or hidden path.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactProviderError::InvalidRequest`] for an empty, oversized or unsafe value.
    pub fn new(value: impl Into<String>) -> Result<Self, ArtifactProviderError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_REFERENCE_BYTES
            || value.starts_with('.')
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            return Err(ArtifactProviderError::InvalidRequest);
        }
        Ok(Self(value))
    }

    /// Returns the validated reference.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Absolute charger-reachable location without embedded user credentials.
///
/// The location is disclosed to stations and diagnostics, so it must not embed a long-lived
/// secret in its path or query either.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactLocation(String);

impl ArtifactLocation {
    /// Validates an `http`, `https`, `ftp` or `ftps` location of at most 512 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactProviderError::InvalidProviderResponse`] for an empty, oversized,
    /// unsupported or credential-bearing location.
    pub fn new(value: impl Into<String>) -> Result<Self, ArtifactProviderError> {
        let value = value.into();
        let lowercase = value.to_ascii_lowercase();
        let authority = LOCATION_SCHEMES
            .iter()
            .find_map(|scheme| lowercase.strip_prefix(scheme))
            .map(|rest| rest.split(['/', '?', '#']).next().unwrap_or_default());
        let valid = value.len() <= MAX_ARTIFACT_LOCATION_BYTES
            && value.bytes().all(|byte| byte.is_ascii_graphic())
            && authority.is_some_and(|authority| !authority.is_empty() && !authority.contains('@'));
        if !valid {
            return Err(ArtifactProviderError::InvalidProviderResponse);
        }
        Ok(Self(value))
    }

    /// Returns the location to place in a station request.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// SHA-256 digest of complete artifact bytes.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct ArtifactSha256([u8; 32]);

impl ArtifactSha256 {
    /// Wraps a digest computed by the provider.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the raw digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for ArtifactSha256 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("sha256:")?;
        self.0
            .iter()
            .try_for_each(|byte| write!(formatter, "{byte:02x}"))
    }
}

impl fmt::Debug for ArtifactSha256 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// Base64 signature over a complete firmware image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareSignatureValue(String);

impl FirmwareSignatureValue {
    /// Validates a non-empty base64 value of at most 800 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactProviderError::InvalidProviderResponse`] for an invalid value.
    pub fn new(value: impl Into<String>) -> Result<Self, ArtifactProviderError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_FIRMWARE_SIGNATURE_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        {
            return Err(ArtifactProviderError::InvalidProviderResponse);
        }
        Ok(Self(value))
    }

    /// Returns the base64 text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Signing certificate and signature that a station verifies before installation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareSignature {
    /// Certificate whose key produced the signature.
    pub signing_certificate: CertificatePem,
    /// Signature over the complete firmware image.
    pub signature: FirmwareSignatureValue,
}

/// Integrity metadata of the genuine artifact bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactIntegrity {
    /// Exact artifact size.
    pub size_bytes: u64,
    /// Digest of the complete artifact.
    pub sha256: ArtifactSha256,
    /// Present exactly for [`ArtifactKind::SignedFirmware`].
    pub signature: Option<FirmwareSignature>,
}

/// Resolved downloadable artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactDescriptor {
    reference: ArtifactReference,
    kind: ArtifactKind,
    location: ArtifactLocation,
    integrity: ArtifactIntegrity,
    test_only: bool,
}

impl ArtifactDescriptor {
    /// Describes a non-empty firmware artifact whose signature matches its kind.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactProviderError::InvalidProviderResponse`] for a log kind, an empty
    /// artifact, or a signature that does not match the kind.
    pub fn new(
        reference: ArtifactReference,
        kind: ArtifactKind,
        location: ArtifactLocation,
        integrity: ArtifactIntegrity,
        test_only: bool,
    ) -> Result<Self, ArtifactProviderError> {
        if !kind.is_firmware()
            || integrity.size_bytes == 0
            || (kind == ArtifactKind::SignedFirmware) != integrity.signature.is_some()
        {
            return Err(ArtifactProviderError::InvalidProviderResponse);
        }
        Ok(Self {
            reference,
            kind,
            location,
            integrity,
            test_only,
        })
    }

    /// Operator-chosen reference that was resolved.
    #[must_use]
    pub const fn reference(&self) -> &ArtifactReference {
        &self.reference
    }

    /// Firmware kind.
    #[must_use]
    pub const fn kind(&self) -> ArtifactKind {
        self.kind
    }

    /// Location to send to the station.
    #[must_use]
    pub const fn location(&self) -> &ArtifactLocation {
        &self.location
    }

    /// Size, digest and optional signature of the genuine bytes.
    #[must_use]
    pub const fn integrity(&self) -> &ArtifactIntegrity {
        &self.integrity
    }

    /// Whether this is demo-only material that production must refuse.
    #[must_use]
    pub const fn test_only(&self) -> bool {
        self.test_only
    }
}

/// Provider-allocated identity of one upload destination.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UploadId(String);

impl UploadId {
    /// Validates 1 to 64 ASCII letters, digits, `-` or `_`.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactProviderError::InvalidRequest`] for an invalid value.
    pub fn new(value: impl Into<String>) -> Result<Self, ArtifactProviderError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_UPLOAD_ID_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(ArtifactProviderError::InvalidRequest);
        }
        Ok(Self(value))
    }

    /// Returns the identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Request for a bounded station log destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UploadRequest {
    /// Log kind; firmware kinds are rejected.
    pub kind: ArtifactKind,
    /// Largest accepted upload; providers may lower but never raise it.
    pub maximum_bytes: u64,
}

/// Destination a station uploads one log file to.
///
/// The location ends with `/` so stations append their file name, as OCPP 2.0.1 N01.FR.21
/// describes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UploadDestination {
    /// Identity used to query the upload's outcome.
    pub upload_id: UploadId,
    /// Requested log kind.
    pub kind: ArtifactKind,
    /// Location to send to the station.
    pub location: ArtifactLocation,
    /// Effective byte cap, never above the requested one.
    pub maximum_bytes: u64,
    /// Whether this is a demo-only destination that production must refuse.
    pub test_only: bool,
}

/// Provider-observed outcome of an upload destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadStatus {
    /// No upload has been attempted yet.
    Pending,
    /// One complete upload was stored.
    Received {
        /// Exact stored size.
        size_bytes: u64,
        /// Digest of the stored bytes.
        sha256: ArtifactSha256,
    },
    /// The latest attempt was refused; the station may retry until one succeeds.
    Refused(UploadRefusal),
}

/// Sanitized reason the latest upload attempt was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadRefusal {
    /// The upload exceeded the destination's byte cap.
    TooLarge,
    /// The upload did not complete before the transfer deadline.
    TimedOut,
    /// The station disconnected or sent a malformed body.
    Interrupted,
    /// The provider could not accept uploads.
    Unavailable,
}

/// Sanitized provider failures without locations, paths or payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactProviderError {
    /// The request was structurally invalid for this port.
    InvalidRequest,
    /// No artifact is published under the reference.
    UnknownArtifact,
    /// No upload destination has the identity.
    UnknownUpload,
    /// The provider is at its bounded capacity.
    Capacity,
    /// The provider cannot currently serve requests.
    Unavailable,
    /// The provider did not answer before its deadline.
    TimedOut,
    /// The provider returned a value that violates this port.
    InvalidProviderResponse,
}

impl fmt::Display for ArtifactProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRequest => "artifact request is invalid",
            Self::UnknownArtifact => "artifact is unknown",
            Self::UnknownUpload => "artifact upload is unknown",
            Self::Capacity => "artifact provider is at capacity",
            Self::Unavailable => "artifact provider is unavailable",
            Self::TimedOut => "artifact provider timed out",
            Self::InvalidProviderResponse => "artifact provider response is invalid",
        })
    }
}

impl Error for ArtifactProviderError {}

/// Provider boundary for firmware downloads and station log uploads.
pub trait ArtifactProvider: Send + Sync {
    /// Describes the provider without exposing configuration or credentials.
    fn descriptor(&self) -> ArtifactProviderDescriptor;

    /// Resolves an operator-chosen firmware reference.
    fn resolve<'a>(
        &'a self,
        reference: &'a ArtifactReference,
    ) -> ArtifactProviderFuture<'a, ArtifactDescriptor>;

    /// Opens one bounded destination for a station log upload.
    fn open_upload(&self, request: UploadRequest) -> ArtifactProviderFuture<'_, UploadDestination>;

    /// Reports the outcome of a destination opened by this provider.
    fn upload_status<'a>(
        &'a self,
        upload: &'a UploadId,
    ) -> ArtifactProviderFuture<'a, UploadStatus>;
}

#[cfg(test)]
mod tests;
