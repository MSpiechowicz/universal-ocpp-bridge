//! Reusable external-database provider conformance support.
//!
//! Concrete adapter tests construct a [`FakeDatabaseHost`], pass its bounded context to the real
//! [`uob_application::DatabaseProvider`], and assert canonical reports through the retained host
//! driver. The harness deliberately exposes no charging command, target, socket, or storage port.

mod descriptor;
mod host;
mod recovery;

pub use descriptor::{DescriptorViolation, inspect_descriptor};
pub use host::{FakeDatabaseHost, HostCapacities, HostContext, HostError};
pub use recovery::{ExportRecoveryLedger, RecoveryDisposition};
