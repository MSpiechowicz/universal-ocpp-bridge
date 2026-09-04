//! Shared fixtures for the listener's own tests.
//!
//! Every router built here goes through the shipped credential parser, the shipped scoped query
//! port, and the shipped router, so a test can only pass by exercising the real authorization and
//! bounding code rather than a stand-in.

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::Value;
use tower::ServiceExt as _;
use uob_application::{
    BridgeTargetFactory, ConfigurationValue, CredentialReference, PageLimit, ScopedTargetQueryPort,
    TargetConfiguration, TargetDescriptor, TargetQueryAuthorization, TargetQueryPermission,
    TargetResourceScope,
};
use uob_contracts::{BridgeId, Environment, StationId, TargetInstanceId};

use crate::{
    capabilities::ListenerLimits,
    configuration::{EmsScadaHttpTargetFactory, IntegrationCredentials, resolve_credentials},
    reads::{ReadExecutor, SupervisedReads},
    routing::{IntegrationState, integration_router},
};

pub(crate) mod fixtures;
mod source;

pub(crate) use source::CanonicalFixtures;

pub(crate) const READER_TOKEN: &str = "reader-token";
pub(crate) const STATION_SCOPED_TOKEN: &str = "station-scoped-token";
pub(crate) const CONTROLLER_TOKEN: &str = "controller-token";
pub(crate) const MULTI_BRIDGE_TOKEN: &str = "multi-bridge-token";

/// Builds the descriptor from the real factory, not a test double.
pub(crate) fn descriptor() -> TargetDescriptor {
    let factory = EmsScadaHttpTargetFactory::new(Environment::Demo);
    let configuration =
        TargetConfiguration::new(TargetInstanceId::new("main").expect("target instance"), 1)
            .with_setting(
                "listen_addr".to_owned(),
                ConfigurationValue::Text("127.0.0.1:9080".to_owned()),
            );
    let validated = <EmsScadaHttpTargetFactory as BridgeTargetFactory<(), ()>>::validate(
        &factory,
        &configuration,
    )
    .expect("validated configuration");
    <EmsScadaHttpTargetFactory as BridgeTargetFactory<(), ()>>::create(&factory, validated)
        .expect("constructed target")
        .descriptor()
}

pub(crate) fn listener_limits(station_scan: u16) -> ListenerLimits {
    ListenerLimits {
        maximum_request_bytes: 64 * 1024,
        maximum_concurrent_requests: 32,
        station_scan_limit: PageLimit::new(station_scan).expect("scan budget"),
    }
}

/// Builds a router whose canonical reads are unavailable, matching an unbound host port.
pub(crate) fn router(credentials: IntegrationCredentials) -> Router {
    router_with(credentials, CanonicalFixtures::default(), 16)
}

/// Builds a router over one canonical fixture set behind the shipped scoped query port.
pub(crate) fn router_with(
    credentials: IntegrationCredentials,
    fixtures: CanonicalFixtures,
    station_scan: u16,
) -> Router {
    let port = ScopedTargetQueryPort::new(Arc::new(fixtures), target_authorization());
    let reads = ReadExecutor::new(
        Arc::new(SupervisedReads::new(Arc::new(port))),
        Duration::from_secs(2),
    );
    integration_router(IntegrationState::new(
        descriptor(),
        listener_limits(station_scan),
        credentials,
        reads,
    ))
}

/// The grant the composition root gives this configured target instance.
///
/// It deliberately covers one station more than any test credential does, so a caller's own scope
/// is the only thing that can keep the extra station out of a response. `station-empty` is granted
/// but holds no snapshot, which is how an in-scope resource can still be absent.
fn target_authorization() -> TargetQueryAuthorization {
    TargetQueryAuthorization::new(
        TargetInstanceId::new("main").expect("target instance"),
        vec![
            TargetQueryPermission::StationSnapshots,
            TargetQueryPermission::DataPoints,
        ],
        [
            "station-a",
            "station-b",
            "station-unscoped",
            "station-empty",
        ]
        .into_iter()
        .map(|station_id| TargetResourceScope::Station {
            bridge_id: BridgeId::new("site-01").expect("bridge identity"),
            station_id: StationId::new(station_id).expect("station identity"),
        })
        .collect(),
    )
}

/// Resolves real credential files so tests exercise the shipped parser and grant validation.
pub(crate) fn scoped_credentials() -> IntegrationCredentials {
    credentials_from(concat!(
        "[[principals]]\nid = 'ems-reader'\ntoken = 'reader-token'\n",
        "permissions = ['read']\nbridges = ['site-01']\n",
        "[[principals]]\nid = 'ems-station-reader'\ntoken = 'station-scoped-token'\n",
        "permissions = ['read']\n",
        "[[principals.stations]]\nbridge_id = 'site-01'\nstation_id = 'station-a'\n",
        "[[principals]]\nid = 'ems-controller'\ntoken = 'controller-token'\n",
        "permissions = ['control']\nbridges = ['site-01']\n",
        "[[principals]]\nid = 'ems-multi-bridge'\ntoken = 'multi-bridge-token'\n",
        "permissions = ['read']\nbridges = ['site-01', 'site-02']\n",
    ))
}

fn credentials_from(document: &str) -> IntegrationCredentials {
    let directory = std::env::temp_dir().join(format!(
        "uob-ems-http-router-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let path = directory.join("integration.toml");
    std::fs::write(&path, document).expect("write credential file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("restrict credential file");
    }
    let credentials = resolve_credentials(
        Some(&CredentialReference::new(path.to_str().expect("path text")).expect("reference")),
        &TargetInstanceId::new("main").expect("target instance"),
    )
    .expect("valid credential file");
    std::fs::remove_dir_all(&directory).expect("remove test directory");
    credentials
}

/// Builds a credentialed router over both canonical resource models.
pub(crate) fn authenticated_router() -> Router {
    router_with(
        scoped_credentials(),
        CanonicalFixtures::both_resource_models(),
        16,
    )
}

pub(crate) async fn send(
    router: Router,
    method: &str,
    path: &str,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = router
        .oneshot(request.body(Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 256 * 1024)
        .await
        .expect("response body");
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, value)
}

pub(crate) async fn get(router: Router, path: &str, token: Option<&str>) -> (StatusCode, Value) {
    send(router, "GET", path, token).await
}
