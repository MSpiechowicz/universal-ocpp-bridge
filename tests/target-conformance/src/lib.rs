//! Reusable target-adapter conformance support.
//!
//! Concrete target tests create a [`FakeTargetHost`], run their real
//! [`uob_application::BridgeTarget`] against the returned context, and drive the target through
//! its protocol-specific peer. The host side remains identical for MQTT, HTTP, and future
//! implementations.

mod descriptor;
mod host;
mod recovery;

pub use descriptor::{DescriptorViolation, inspect_descriptor};
pub use host::{
    CommandSubmission, FakeTargetHost, HostCapacities, HostContext, HostError,
    UnsupportedQueryPort, target_port_error_from_admission,
};
pub use recovery::{DeliveryRecoveryLedger, RecoveryDisposition};
