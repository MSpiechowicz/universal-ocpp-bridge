#![doc = "Protocol adapter boundary. Concrete protocol model crates belong here."]

mod endpoint;
mod error;
mod security;
mod station;
pub mod v16;
pub mod v201;

pub use endpoint::{
    OcppEndpoint, StationConnection, StationConnectionReceiver, StationEndpointConfigurationError,
    StationEndpointServeError,
};
pub use error::{DecodeError, DecodeErrorKind, OcppCallError, OcppErrorCode};
pub use security::{
    AuthenticatedStation, ResolvedStationCredential, StationAuthenticationMode,
    StationAuthenticator, StationCertificateFingerprint, StationCredential, StationRegistration,
    StationSecurityConfiguration, StationSecurityConfigurationError,
    StationSecurityConfigurationErrorCode, StationTlsAcceptor, StationTlsHandshakeError,
    StationTlsMaterial, StationTlsReferences, StationTransportAdmissionError,
    StationTransportAdmissionRequest, ValidatedStationSecurityConfiguration,
    build_station_tls_acceptor,
};

pub use station::{
    OrderedStationOutput, SpawnedStation, StationHandle, StationOutputKind, StationOutputReceiver,
    StationOutputs, StationSendError, StationTask, StationTaskError, spawn_station,
};

/// Marker implemented by protocol adapters registered by the service.
pub trait ProtocolAdapter: Send + Sync {
    /// Stable adapter kind used by the composition root.
    fn kind(&self) -> &'static str;
}

/// A validated charger-originated OCPP CALL converted to an application-owned operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedCall {
    /// OCPP unique message identifier used for the corresponding response or error.
    pub message_id: String,
    /// Exact OCPP action from the incoming frame.
    pub action: uob_contracts::ProtocolActionName,
    /// Target-neutral operation passed into application coordination.
    pub observation: uob_application::ChargerObservation,
}

/// Selects one of the only WebSocket subprotocols supported by this release.
#[must_use]
pub fn protocol_for_subprotocol(value: &str) -> Option<uob_contracts::ProtocolEdition> {
    match value {
        "ocpp1.6" => Some(uob_contracts::ProtocolEdition::Ocpp16j),
        "ocpp2.0.1" => Some(uob_contracts::ProtocolEdition::Ocpp201),
        _ => None,
    }
}
