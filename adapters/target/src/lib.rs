#![doc = "Target adapter composition, catalog, and offline configuration validation."]

mod catalog;
mod delivery;
mod destination;
mod registry;
mod security;
mod session;

pub use catalog::{TargetCatalogEntry, TargetDisplayFamily, TargetPreset, TargetRegistration};
pub use delivery::{
    DeliveryRetryPolicy, StoredTargetMessage, TargetDeliveryReportReceiver,
    TargetDeliveryWorkerError, TargetDeliveryWorkerOptions, TargetDeliveryWorkerTask,
    spawn_target_delivery_worker, target_delivery_reports,
};
pub use destination::{
    AuditedTargetDisposition, TargetBacklogEntry, TargetBacklogState, TargetChangeError,
    TargetConfigurationPreview, TargetDestination, TargetDispositionAction,
};
pub use registry::{
    BridgeTargetSelection, ConfiguredTarget, EMS_SCADA_OPCUA_KIND, RegistrationError,
    TargetRegistry, TargetSelectionError, ValidatedTargetSelection,
};
pub use security::{
    EndpointError, NetworkEndpoint, TransportEncryption, TransportPolicy, TransportPolicyError,
    TransportSecurity, validate_transport_security,
};
pub use session::{
    TargetDeliveryIngress, TargetDeliveryIngressError, TargetSessionError, TargetSessionOptions,
    TargetSessionPorts, TargetSessionTask, spawn_target_session,
};
