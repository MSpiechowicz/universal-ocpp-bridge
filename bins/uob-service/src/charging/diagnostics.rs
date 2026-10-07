//! Demo-only OCPP 1.6 diagnostics and log upload composition. Stations upload to the shared
//! local test artifact service; every destination is test-only and production is refused.
use std::sync::Arc;

use uob_application::DiagnosticsStore16;
use uob_contracts::{Operation, ProtocolEdition, StationSnapshot, SupportedOperation};
use uob_protocol_adapter::v16::remote_control::{DiagnosticsSettings16, ReservationGrant16};

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

/// Offers exactly the configured native families on the station root.
pub(super) fn apply_capabilities(
    snapshot: &mut StationSnapshot,
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
                    protocol: ProtocolEdition::Ocpp16j,
                    action: action.to_owned(),
                },
                parameters: vec![],
            });
        }
    }
}
