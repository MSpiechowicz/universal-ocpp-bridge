use std::{collections::BTreeMap, error::Error, fmt};

use rustls::pki_types::CertificateDer;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uob_application::CredentialReference;
use uob_contracts::{ProtocolEdition, StationId};

use crate::protocol_for_subprotocol;

mod tls;

pub use tls::{
    StationTlsAcceptor, StationTlsHandshakeError, StationTlsMaterial, build_station_tls_acceptor,
};

const MINIMUM_STATION_CREDENTIAL_BYTES: usize = 16;

/// Authentication required after the TLS handshake and before WebSocket admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StationAuthenticationMode {
    /// TLS plus one high-entropy credential unique to the configured station.
    Credential,
    /// TLS, a unique station credential, and a required trusted client certificate.
    CredentialAndMutualTls,
}

/// Credential-file references for the OCPP TLS listener.
#[derive(Clone, Eq, PartialEq)]
pub struct StationTlsReferences {
    /// PEM certificate chain presented by the service.
    pub server_certificate_chain: CredentialReference,
    /// PEM private key matching the service certificate.
    pub server_private_key: CredentialReference,
    /// PEM trust anchors for client certificates when mutual TLS is selected.
    pub client_certificate_authorities: Option<CredentialReference>,
}

impl fmt::Debug for StationTlsReferences {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StationTlsReferences")
            .field("server_certificate_chain", &"[redacted]")
            .field("server_private_key", &"[redacted]")
            .field(
                "client_certificate_authorities",
                &self
                    .client_certificate_authorities
                    .as_ref()
                    .map(|_| "[redacted]"),
            )
            .finish()
    }
}

/// SHA-256 binding for a station's end-entity client certificate.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct StationCertificateFingerprint([u8; 32]);

impl StationCertificateFingerprint {
    /// Calculates the binding from DER certificate bytes.
    #[must_use]
    pub fn from_der(certificate: &[u8]) -> Self {
        Self(Sha256::digest(certificate).into())
    }
}

impl fmt::Debug for StationCertificateFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StationCertificateFingerprint([redacted])")
    }
}

/// One preconfigured station identity and its secret/certificate references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StationRegistration {
    /// Stable identity used in the OCPP path and application state.
    pub station_id: StationId,
    /// Secret resolved only while constructing the runtime authenticator.
    pub credential: CredentialReference,
    /// Required end-entity client certificate binding in mutual-TLS mode.
    pub client_certificate: Option<StationCertificateFingerprint>,
}

/// Unvalidated transport policy for every OCPP station listener.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StationSecurityConfiguration {
    /// Whether client certificates are required in addition to credentials.
    pub authentication: StationAuthenticationMode,
    /// TLS material references resolved outside offline configuration validation.
    pub tls: StationTlsReferences,
    /// Complete local station allowlist.
    pub stations: Vec<StationRegistration>,
}

/// Validated station policy that cannot be constructed without a nonempty allowlist.
#[derive(Clone, Debug)]
pub struct ValidatedStationSecurityConfiguration {
    authentication: StationAuthenticationMode,
    tls: StationTlsReferences,
    stations: BTreeMap<StationId, StationRegistration>,
}

impl StationSecurityConfiguration {
    /// Validates references, identity uniqueness, and mTLS bindings without reading secrets.
    ///
    /// # Errors
    ///
    /// Returns a stable field-only error and never includes credential or certificate contents.
    pub fn validate(
        self,
    ) -> Result<ValidatedStationSecurityConfiguration, StationSecurityConfigurationError> {
        let expects_certificates =
            self.authentication == StationAuthenticationMode::CredentialAndMutualTls;
        if expects_certificates != self.tls.client_certificate_authorities.is_some() {
            return Err(StationSecurityConfigurationError::new(
                StationSecurityConfigurationErrorCode::ClientCertificateAuthorities,
                "tls.client_certificate_authorities",
            ));
        }
        if self.stations.is_empty() {
            return Err(StationSecurityConfigurationError::new(
                StationSecurityConfigurationErrorCode::EmptyStationAllowlist,
                "stations",
            ));
        }

        let mut stations = BTreeMap::new();
        let mut credential_references = BTreeMap::new();
        let mut certificate_bindings = BTreeMap::new();
        for station in self.stations {
            if expects_certificates != station.client_certificate.is_some() {
                return Err(StationSecurityConfigurationError::new(
                    StationSecurityConfigurationErrorCode::ClientCertificateBinding,
                    "stations.client_certificate",
                ));
            }
            if credential_references
                .insert(
                    station.credential.as_str().to_owned(),
                    station.station_id.clone(),
                )
                .is_some()
            {
                return Err(StationSecurityConfigurationError::new(
                    StationSecurityConfigurationErrorCode::SharedCredentialReference,
                    "stations.credential",
                ));
            }
            if let Some(binding) = &station.client_certificate
                && certificate_bindings
                    .insert(binding.clone(), station.station_id.clone())
                    .is_some()
            {
                return Err(StationSecurityConfigurationError::new(
                    StationSecurityConfigurationErrorCode::SharedClientCertificate,
                    "stations.client_certificate",
                ));
            }
            if stations
                .insert(station.station_id.clone(), station)
                .is_some()
            {
                return Err(StationSecurityConfigurationError::new(
                    StationSecurityConfigurationErrorCode::DuplicateStation,
                    "stations.station_id",
                ));
            }
        }

        Ok(ValidatedStationSecurityConfiguration {
            authentication: self.authentication,
            tls: self.tls,
            stations,
        })
    }
}

impl ValidatedStationSecurityConfiguration {
    /// Returns whether trusted client certificates are mandatory.
    #[must_use]
    pub const fn authentication(&self) -> StationAuthenticationMode {
        self.authentication
    }

    /// Returns credential references for the listener TLS material.
    #[must_use]
    pub const fn tls(&self) -> &StationTlsReferences {
        &self.tls
    }

    /// Returns the number of identities in the local allowlist.
    #[must_use]
    pub fn station_count(&self) -> usize {
        self.stations.len()
    }
}

/// Stable category for an invalid station security configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StationSecurityConfigurationErrorCode {
    /// Mutual TLS and its CA reference were configured inconsistently.
    ClientCertificateAuthorities,
    /// No station can be admitted because the allowlist is empty.
    EmptyStationAllowlist,
    /// A station identity was declared more than once.
    DuplicateStation,
    /// More than one station points at the same credential source.
    SharedCredentialReference,
    /// A required binding is missing or an ignored binding was supplied.
    ClientCertificateBinding,
    /// More than one station is bound to the same client certificate.
    SharedClientCertificate,
    /// A resolved credential is missing, unknown, duplicated, or too short.
    ResolvedCredential,
    /// Two configured stations resolved to the same secret.
    SharedResolvedCredential,
    /// Loaded server certificate chain and private key are absent, invalid, or mismatched.
    ServerTlsMaterial,
}

/// Sanitized offline configuration failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StationSecurityConfigurationError {
    code: StationSecurityConfigurationErrorCode,
    field: &'static str,
}

impl StationSecurityConfigurationError {
    const fn new(code: StationSecurityConfigurationErrorCode, field: &'static str) -> Self {
        Self { code, field }
    }

    /// Returns the stable machine-readable category.
    #[must_use]
    pub const fn code(&self) -> StationSecurityConfigurationErrorCode {
        self.code
    }

    /// Returns only the safe configuration field name.
    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }
}

impl fmt::Display for StationSecurityConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid station security configuration at {}: {:?}",
            self.field, self.code
        )
    }
}

impl Error for StationSecurityConfigurationError {}

/// Secret credential converted immediately to a non-reversible comparison digest.
#[derive(Eq, PartialEq)]
pub struct StationCredential([u8; 32]);

impl StationCredential {
    /// Hashes a high-entropy station credential for constant-time runtime comparison.
    ///
    /// # Errors
    ///
    /// Rejects credentials shorter than 16 bytes without retaining or echoing their contents.
    pub fn from_secret(secret: &[u8]) -> Result<Self, StationSecurityConfigurationError> {
        if secret.len() < MINIMUM_STATION_CREDENTIAL_BYTES {
            return Err(StationSecurityConfigurationError::new(
                StationSecurityConfigurationErrorCode::ResolvedCredential,
                "stations.credential",
            ));
        }
        Ok(Self(Sha256::digest(secret).into()))
    }

    fn matches(&self, presented: &[u8]) -> bool {
        let candidate: [u8; 32] = Sha256::digest(presented).into();
        bool::from(self.0.ct_eq(&candidate))
    }
}

impl fmt::Debug for StationCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StationCredential([redacted])")
    }
}

/// One credential resolved by the composition root from the configured reference.
#[derive(Debug)]
pub struct ResolvedStationCredential {
    /// Station whose credential reference was resolved.
    pub station_id: StationId,
    /// One-way digest of the resolved secret.
    pub credential: StationCredential,
}

#[derive(Debug)]
struct RuntimeStation {
    registration: StationRegistration,
    credential: StationCredential,
}

/// Complete immutable station allowlist used before allocating station runtime state.
#[derive(Debug)]
pub struct StationAuthenticator {
    authentication: StationAuthenticationMode,
    stations: BTreeMap<StationId, RuntimeStation>,
}

impl StationAuthenticator {
    /// Joins validated registrations with resolved secret digests and rejects all ambiguity.
    ///
    /// # Errors
    ///
    /// Every configured station must have exactly one unique resolved credential.
    pub fn new(
        configuration: ValidatedStationSecurityConfiguration,
        credentials: Vec<ResolvedStationCredential>,
    ) -> Result<Self, StationSecurityConfigurationError> {
        let mut resolved = BTreeMap::new();
        for credential in credentials {
            if !configuration.stations.contains_key(&credential.station_id)
                || resolved
                    .insert(credential.station_id, credential.credential)
                    .is_some()
            {
                return Err(StationSecurityConfigurationError::new(
                    StationSecurityConfigurationErrorCode::ResolvedCredential,
                    "stations.credential",
                ));
            }
        }
        if resolved.len() != configuration.stations.len() {
            return Err(StationSecurityConfigurationError::new(
                StationSecurityConfigurationErrorCode::ResolvedCredential,
                "stations.credential",
            ));
        }

        let digests: Vec<&[u8; 32]> = resolved.values().map(|value| &value.0).collect();
        for (index, digest) in digests.iter().enumerate() {
            if digests[index + 1..].contains(digest) {
                return Err(StationSecurityConfigurationError::new(
                    StationSecurityConfigurationErrorCode::SharedResolvedCredential,
                    "stations.credential",
                ));
            }
        }

        let mut stations = BTreeMap::new();
        for (station_id, registration) in configuration.stations {
            let Some(credential) = resolved.remove(&station_id) else {
                return Err(StationSecurityConfigurationError::new(
                    StationSecurityConfigurationErrorCode::ResolvedCredential,
                    "stations.credential",
                ));
            };
            stations.insert(
                station_id,
                RuntimeStation {
                    registration,
                    credential,
                },
            );
        }
        Ok(Self {
            authentication: configuration.authentication,
            stations,
        })
    }

    /// Verifies identity, subprotocol, credential, and optional certificate binding.
    ///
    /// # Errors
    ///
    /// Returns only a stable denial category; presented secrets are never retained or formatted.
    pub fn authenticate(
        &self,
        request: StationTransportAdmissionRequest<'_>,
    ) -> Result<AuthenticatedStation, StationTransportAdmissionError> {
        let protocol = protocol_for_subprotocol(request.websocket_subprotocol)
            .ok_or(StationTransportAdmissionError::UnsupportedProtocol)?;
        let station = self
            .stations
            .get(request.station_id)
            .ok_or(StationTransportAdmissionError::UnknownStation)?;
        let presented = request
            .credential
            .ok_or(StationTransportAdmissionError::MissingCredential)?;
        if !station.credential.matches(presented) {
            return Err(StationTransportAdmissionError::InvalidCredential);
        }
        if self.authentication == StationAuthenticationMode::CredentialAndMutualTls {
            let certificate = request
                .peer_certificates
                .first()
                .ok_or(StationTransportAdmissionError::MissingClientCertificate)?;
            let binding = StationCertificateFingerprint::from_der(certificate.as_ref());
            if station.registration.client_certificate.as_ref() != Some(&binding) {
                return Err(StationTransportAdmissionError::ClientCertificateBinding);
            }
        }
        Ok(AuthenticatedStation {
            station_id: station.registration.station_id.clone(),
            protocol,
        })
    }
}

/// Borrowed values collected from a completed TLS handshake and WebSocket request.
#[derive(Clone, Copy)]
pub struct StationTransportAdmissionRequest<'a> {
    /// Station path identity supplied by the remote peer.
    pub station_id: &'a StationId,
    /// Exact negotiated OCPP WebSocket subprotocol.
    pub websocket_subprotocol: &'a str,
    /// Credential from the authenticated transport header, absent if not supplied.
    pub credential: Option<&'a [u8]>,
    /// Peer chain already validated by rustls in mutual-TLS mode.
    pub peer_certificates: &'a [CertificateDer<'static>],
}

/// Trusted station context passed to the future WebSocket endpoint and application layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedStation {
    /// Configured station identity, never an unchecked request string.
    pub station_id: StationId,
    /// One of the two supported OCPP editions.
    pub protocol: ProtocolEdition,
}

/// Stable authentication denial safe for external responses and bounded diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StationTransportAdmissionError {
    /// Requested station is not registered locally.
    UnknownStation,
    /// Requested WebSocket subprotocol is unsupported.
    UnsupportedProtocol,
    /// No station credential was presented.
    MissingCredential,
    /// Credential does not belong to the requested station.
    InvalidCredential,
    /// Mutual TLS was required but no peer certificate reached admission.
    MissingClientCertificate,
    /// Trusted certificate belongs to another configured station.
    ClientCertificateBinding,
}

impl fmt::Display for StationTransportAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("station transport admission denied")
    }
}

impl Error for StationTransportAdmissionError {}
