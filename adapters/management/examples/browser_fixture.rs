//! Loopback-only real management-router fixture for console browser acceptance.
//! No fixture controls or credentials are compiled into the production service.
#[path = "browser_fixture/diagnostics.rs"]
mod diagnostics;
#[path = "browser_fixture/source.rs"]
mod source;

use std::{net::SocketAddr, sync::Arc, time::Duration};
use uob_application::{
    Application, TargetQueryAuthorization, TargetQueryPermission, TargetResourceScope,
};
use uob_contracts::{
    ArtifactDigest, BridgeId, Environment, ProcessInstanceId, ReleaseId, RuntimeIdentity,
    ServiceIdentity, TargetInstanceId,
};
use uob_management_adapter::{
    AuthenticatedEventAccess, ManagementEventAuthenticator, ManagementEventConfiguration,
    ManagementEventLimits, ManagementReadLimits, ManagementRouterOptions,
    router_with_authenticated_events,
};

struct Authenticator;
impl ManagementEventAuthenticator for Authenticator {
    fn authenticate(&self, token: &str) -> Option<AuthenticatedEventAccess> {
        // Public deterministic fixture credential, never a deployed credential.
        (token == "browser-fixture-reader").then(|| AuthenticatedEventAccess {
            authorization: TargetQueryAuthorization::new(
                TargetInstanceId::new("browser-fixture").unwrap(),
                vec![
                    TargetQueryPermission::StationSnapshots,
                    TargetQueryPermission::RetainedEvents,
                ],
                vec![TargetResourceScope::Station {
                    bridge_id: source::resource().bridge_id,
                    station_id: source::resource().station_id,
                }],
            ),
            default_resource: source::resource(),
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let port: u16 = std::env::var("UOB_BROWSER_TEST_PORT")
        .unwrap_or_else(|_| "39189".into())
        .parse()
        .unwrap();
    let environment = if port == 39190 {
        Environment::Staging
    } else {
        Environment::Production
    };
    let runtime = RuntimeIdentity {
        environment,
        release_id: ReleaseId::new("release-browser-fixture").unwrap(),
        release_digest: ArtifactDigest::new("sha256:browser-fixture").unwrap(),
        process_instance_id: ProcessInstanceId::new("process-browser-fixture").unwrap(),
    };
    let application = Application::new(ServiceIdentity {
        bridge_id: BridgeId::new("bridge-browser-fixture").unwrap(),
        runtime: runtime.clone(),
        selected_target_id: None,
    });
    let identity = application.identity().clone();
    let router = router_with_authenticated_events(
        application,
        Arc::new(source::Source { runtime }),
        ManagementReadLimits::default(),
        ManagementEventConfiguration {
            authenticator: Arc::new(Authenticator),
            limits: ManagementEventLimits {
                keep_alive_interval: Duration::from_secs(1),
                ..ManagementEventLimits::default()
            },
        },
        ManagementRouterOptions {
            static_assets: port != 39191,
        },
    );
    let router = router.merge(uob_management_adapter::capture_router(
        identity.clone(),
        diagnostics::configuration(identity),
    ));
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))
        .await
        .unwrap();
    axum::serve(listener, router).await.unwrap();
}
