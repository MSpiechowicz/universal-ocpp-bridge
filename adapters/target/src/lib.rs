#![doc = "Target adapter composition, catalog, and offline configuration validation."]

mod catalog;
mod registry;
mod security;

pub use catalog::{TargetCatalogEntry, TargetDisplayFamily, TargetPreset, TargetRegistration};
pub use registry::{
    BridgeTargetSelection, ConfiguredTarget, EMS_SCADA_OPCUA_KIND, RegistrationError,
    TargetRegistry, TargetSelectionError, ValidatedTargetSelection,
};
pub use security::{
    EndpointError, NetworkEndpoint, TransportEncryption, TransportPolicy, TransportPolicyError,
    TransportSecurity, validate_transport_security,
};
