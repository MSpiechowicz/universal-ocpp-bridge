//! Demo-only OCPP 1.6 diagnostics and OCPP 1.6/2.0.1 log upload composition. Stations upload to
//! the shared local test artifact service; every destination is test-only and production is
//! refused.
use std::sync::Arc;

use uob_application::{DiagnosticsStore16, DiagnosticsStore201};
use uob_contracts::{Operation, ProtocolEdition, StationSnapshot, SupportedOperation};
use uob_protocol_adapter::{
    v16::remote_control::{DiagnosticsSettings16, ReservationGrant16},
    v201::remote_control::{DiagnosticsSettings201, ReservationGrant201},
};

use super::firmware::Providers;
use crate::configuration::charging::StationDiagnostics;

/// Per-socket policy for one diagnostics-enabled station.
pub(super) fn session(
    station: StationDiagnostics,
    providers: &Providers,
    jobs: Arc<dyn DiagnosticsStore16>,
    grant: Arc<ReservationGrant16>,
) -> Arc<DiagnosticsSettings16> {
    Arc::new(DiagnosticsSettings16 {
        diagnostics: station.diagnostics,
        log: station.log,
        job_timeout: station.job_timeout,
        maximum_upload_bytes: station.maximum_upload_bytes,
        artifacts: Arc::new(providers.artifacts().clone()),
        jobs,
        policy: providers.policy(),
        grant,
    })
}

/// Per-socket policy for one OCPP 2.0.1 log-enabled station.
pub(super) fn session_201(
    station: StationDiagnostics,
    providers: &Providers,
    jobs: Arc<dyn DiagnosticsStore201>,
    grant: Arc<ReservationGrant201>,
) -> Arc<DiagnosticsSettings201> {
    Arc::new(DiagnosticsSettings201 {
        job_timeout: station.job_timeout,
        maximum_upload_bytes: station.maximum_upload_bytes,
        artifacts: Arc::new(providers.artifacts().clone()),
        jobs,
        policy: providers.policy(),
        grant,
    })
}

/// Offers exactly the configured native families on the station root.
pub(super) fn apply_capabilities(
    snapshot: &mut StationSnapshot,
    protocol: ProtocolEdition,
    diagnostics: Option<StationDiagnostics>,
) {
    let Some(diagnostics) = diagnostics else {
        return;
    };
    for (enabled, action) in [
        (diagnostics.diagnostics, "GetDiagnostics"),
        (diagnostics.log, "GetLog"),
    ] {
        if enabled {
            snapshot.capabilities.operations.push(SupportedOperation {
                operation: Operation::ProtocolAction {
                    protocol,
                    action: action.to_owned(),
                },
                parameters: vec![],
            });
        }
    }
}
