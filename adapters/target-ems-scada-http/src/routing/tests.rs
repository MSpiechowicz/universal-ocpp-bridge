use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::Value;
use tower::ServiceExt as _;
use uob_application::{
    BridgeTargetFactory, ConfigurationValue, CredentialReference, TargetConfiguration,
    TargetDescriptor,
};
use uob_contracts::{Environment, TargetInstanceId};

use super::{IntegrationState, integration_router};
use crate::{
    capabilities::IMPLEMENTED_RESOURCES,
    configuration::{EmsScadaHttpTargetFactory, IntegrationCredentials, resolve_credentials},
};

const READER_TOKEN: &str = "reader-token";

/// Builds the descriptor and listener limits from the real factory, not a test double.
fn descriptor() -> TargetDescriptor {
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

fn router(credentials: IntegrationCredentials) -> Router {
    integration_router(IntegrationState::new(
        descriptor(),
        crate::capabilities::ListenerLimits {
            maximum_request_bytes: 64 * 1024,
            maximum_concurrent_requests: 32,
        },
        credentials,
    ))
}

fn authenticated_router() -> Router {
    router(scoped_credentials())
}

/// Resolves one real credential file so tests exercise the shipped parser and grant validation.
fn scoped_credentials() -> IntegrationCredentials {
    let directory = std::env::temp_dir().join(format!(
        "uob-ems-http-router-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let path = directory.join("integration.toml");
    std::fs::write(
        &path,
        "[[principals]]\nid = 'ems-reader'\ntoken = 'reader-token'\npermissions = ['read']\nbridges = ['site-01']\n",
    )
    .expect("write credential file");
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

async fn send(
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
    let body = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("response body");
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, value)
}

async fn get(router: Router, path: &str, token: Option<&str>) -> (StatusCode, Value) {
    send(router, "GET", path, token).await
}

#[tokio::test]
async fn the_capability_response_advertises_exactly_the_routes_this_build_serves() {
    let (status, body) = get(
        router(IntegrationCredentials::default()),
        "/bridge/v1/capabilities",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let resources = body["resources"].as_array().expect("resource list");
    assert_eq!(resources.len(), IMPLEMENTED_RESOURCES.len());
    for resource in resources {
        let path = resource["path"].as_str().expect("resource path");
        let (status, _) = get(router(IntegrationCredentials::default()), path, None).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "advertised resource {path} is not served"
        );
    }
    assert_eq!(resources[0]["path"], "/bridge/v1/capabilities");
    assert_eq!(resources[0]["operations"], serde_json::json!(["read"]));
}

#[tokio::test]
async fn the_capability_response_matches_the_supervised_target_descriptor() {
    let descriptor = descriptor();
    let (_, body) = get(
        router(IntegrationCredentials::default()),
        "/bridge/v1/capabilities",
        None,
    )
    .await;

    assert_eq!(
        body["contract_version"]["major"],
        u64::from(descriptor.contract_version.major)
    );
    assert_eq!(body["target"]["kind"], descriptor.kind.as_str());
    assert_eq!(
        body["target"]["instance_id"],
        descriptor.instance_id.as_str()
    );
    assert_eq!(
        body["outbound_message_classes"],
        serde_json::json!(["station_snapshot", "domain_event", "command_result"])
    );
    assert_eq!(
        body["delivery_semantics"],
        serde_json::json!(["local_exposure"])
    );
    // Command admission is a separate integration endpoint, so nothing inbound is advertised.
    assert!(descriptor.inbound_operations.is_empty());
    assert_eq!(body["inbound_operations"], serde_json::json!([]));
    assert_eq!(
        body["limits"]["maximum_message_bytes"],
        descriptor.limits.maximum_message_bytes
    );
    assert_eq!(
        body["limits"]["maximum_in_flight_deliveries"],
        descriptor.limits.maximum_in_flight_deliveries
    );
    assert_eq!(
        body["limits"]["maximum_in_flight_commands"],
        descriptor.limits.maximum_in_flight_commands
    );
}

#[tokio::test]
async fn management_debug_and_simulator_paths_are_unreachable_with_a_stable_code() {
    for path in [
        "/api/v1/health",
        "/api/v1/identity",
        "/api/v1/commands",
        "/api/v1/events",
        "/api/v1/stations",
        "/metrics",
        "/health",
        "/",
        "/bridge/v1",
        "/bridge/v2/capabilities",
    ] {
        let (status, body) = get(router(IntegrationCredentials::default()), path, None).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{path} must not be served here"
        );
        assert_eq!(body["error"], "ems_scada_http.unknown_resource", "{path}");
    }
}

#[tokio::test]
async fn an_unsupported_method_is_reported_separately_from_an_unknown_resource() {
    let (status, body) = send(
        router(IntegrationCredentials::default()),
        "POST",
        "/bridge/v1/capabilities",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body["error"], "ems_scada_http.unsupported_operation");
}

#[tokio::test]
async fn a_credentialed_listener_authenticates_every_integration_request() {
    let (status, body) = get(authenticated_router(), "/bridge/v1/capabilities", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "ems_scada_http.unauthenticated");

    let (status, body) = get(
        authenticated_router(),
        "/bridge/v1/capabilities",
        Some("not-the-token"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "ems_scada_http.invalid_credential");
}

#[tokio::test]
async fn an_authenticated_caller_sees_only_its_own_scope() {
    let (status, body) = get(
        authenticated_router(),
        "/bridge/v1/capabilities",
        Some(READER_TOKEN),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["caller"]["permissions"], serde_json::json!(["read"]));
    assert_eq!(
        body["caller"]["resource_scopes"],
        serde_json::json!([{"scope": "bridge", "bridge_id": "site-01"}])
    );
}

#[tokio::test]
async fn an_unknown_resource_is_refused_before_credentials_are_considered() {
    let (status, body) = get(
        authenticated_router(),
        "/api/v1/identity",
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "ems_scada_http.unknown_resource");
}
