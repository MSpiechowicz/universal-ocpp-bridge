//! Explicitly enabled synthetic capture producer for real-router browser tests only.
use std::{sync::Arc, time::Duration};
use uob_application::{
    CommandClock, FlowDiagnostics, FlowEvidence, FlowStage,
    capture::{CaptureGrant, CaptureManager, CapturePermission},
};
use uob_contracts::{CorrelationId, ServiceIdentity, StationId, UtcTimestamp};
use uob_management_adapter::{ManagementCaptureAuthenticator, ManagementCaptureConfiguration};

struct Auth(ServiceIdentity);
impl ManagementCaptureAuthenticator for Auth {
    fn authenticate(&self, token: &str) -> Option<CaptureGrant> {
        if !uob_management_adapter::token_matches_environment(token, self.0.runtime.environment) {
            return None;
        }
        let environment = match self.0.runtime.environment {
            uob_contracts::Environment::Production => "production",
            uob_contracts::Environment::Staging => "staging",
            uob_contracts::Environment::Demo => "demo",
        };
        let operator =
            format!("uob1.{environment}.browser-fixture-diagnostics-{environment}-secret");
        let reader =
            format!("uob1.{environment}.browser-fixture-diagnostics-reader-{environment}-secret");
        let permissions = match token {
            value if value == operator => {
                vec![CapturePermission::Read, CapturePermission::Capture]
            }
            value if value == reader => vec![CapturePermission::Read],
            _ => return None,
        };
        CaptureGrant::new(self.0.bridge_id.clone(), permissions, None, None).ok()
    }
}
struct Clock;
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        serde_json::from_str("\"2026-09-12T12:00:00Z\"").unwrap()
    }
}
pub fn configuration(identity: ServiceIdentity) -> ManagementCaptureConfiguration {
    let manager = CaptureManager::with_ring_limits(true, 8 * 1024 * 1024, 2000).unwrap();
    let flow = FlowDiagnostics::retained(
        identity.runtime.process_instance_id.clone(),
        identity.bridge_id.clone(),
        manager.clone(),
        Arc::new(Clock),
    );
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tick.tick().await;
            flow.span(
                Some(CorrelationId::new("browser-synthetic-correlation").unwrap()),
                Some(StationId::new("station-browser-fixture").unwrap()),
                None,
            )
            .emit(FlowStage::Application, FlowEvidence::Completed);
        }
    });
    ManagementCaptureConfiguration {
        manager,
        authenticator: Arc::new(Auth(identity)),
    }
}
