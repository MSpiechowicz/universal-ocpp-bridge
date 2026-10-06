use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use tower::ServiceExt;
use uob_application::{AccessGrant, AccessPermission, AccessResourceScope};
use uob_contracts::{
    ArtifactDigest, AuthenticatedCommandOrigin, BridgeId, Environment, PrincipalId,
    ProcessInstanceId, ReleaseId, RuntimeIdentity, ServiceIdentity, StationId, TargetInstanceId,
};
use uob_management_adapter::{
    ConfigurationApiError, ConfigurationFuture, DestinationView, DispositionAction,
    ManagementConfigurationAuthenticator, ManagementConfigurationPort,
    ManagementConfigurationSetup, TargetApplyOutcome, TargetApplyRequest, TargetCatalogView,
    TargetConfigurationCandidate, TargetConfigurationView, TargetDispositionRequest,
    TargetDispositionView, TargetSectionView, TargetValidationReport, configuration_router,
};

fn identity() -> ServiceIdentity {
    ServiceIdentity {
        bridge_id: BridgeId::new("bridge-a").unwrap(),
        runtime: RuntimeIdentity {
            environment: Environment::Demo,
            release_id: ReleaseId::new("release-a").unwrap(),
            release_digest: ArtifactDigest::new("sha256:a").unwrap(),
            process_instance_id: ProcessInstanceId::new("process-a").unwrap(),
        },
        selected_target_id: Some(TargetInstanceId::new("main").unwrap()),
    }
}

fn grant(principal: &str, permissions: Vec<AccessPermission>, bridge_wide: bool) -> AccessGrant {
    let scope = if bridge_wide {
        AccessResourceScope::Bridge(BridgeId::new("bridge-a").unwrap())
    } else {
        AccessResourceScope::Station {
            bridge_id: BridgeId::new("bridge-a").unwrap(),
            station_id: StationId::new("station-a").unwrap(),
        }
    };
    AccessGrant::new(
        AuthenticatedCommandOrigin::Management {
            principal_id: PrincipalId::new(principal).unwrap(),
        },
        permissions,
        vec![scope],
    )
    .unwrap()
}

struct Tokens(BTreeMap<&'static str, AccessGrant>);

impl ManagementConfigurationAuthenticator for Tokens {
    fn authenticate(&self, token: &str) -> Option<AccessGrant> {
        self.0.get(token).cloned()
    }
}

#[derive(Default)]
struct Port {
    calls: Mutex<Vec<String>>,
    invalid: bool,
}

fn destination(revision: u64) -> DestinationView {
    DestinationView {
        target_id: "main".to_owned(),
        configuration_revision: revision,
    }
}

fn report(valid: bool) -> TargetValidationReport {
    TargetValidationReport {
        valid,
        errors: vec![],
        configuration_digest: "sha256:digest".to_owned(),
        assigned_revisions: BTreeMap::new(),
        running_destination: Some(destination(1)),
        next_destination: Some(destination(2)),
        restart_required: true,
        pending_critical_deliveries: 0,
        blocking_destinations: vec![],
        dispositions: vec![],
    }
}

impl ManagementConfigurationPort for Port {
    fn catalog(&self) -> ConfigurationFuture<'_, TargetCatalogView> {
        self.calls.lock().unwrap().push("catalog".to_owned());
        Box::pin(async { Ok(TargetCatalogView { kinds: vec![] }) })
    }

    fn current(&self) -> ConfigurationFuture<'_, TargetConfigurationView> {
        self.calls.lock().unwrap().push("current".to_owned());
        Box::pin(async {
            Ok(TargetConfigurationView {
                bridge_id: "bridge-a".to_owned(),
                environment: Environment::Demo,
                configuration_digest: "sha256:digest".to_owned(),
                running_destination: Some(destination(1)),
                next_destination: Some(destination(1)),
                restart_required: false,
                next_start: TargetSectionView {
                    source: "base",
                    target_id: Some("main".to_owned()),
                    targets: vec![],
                },
                backlog: vec![],
                dispositions: vec![],
            })
        })
    }

    fn validate(
        &self,
        candidate: TargetConfigurationCandidate,
    ) -> ConfigurationFuture<'_, TargetValidationReport> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("validate:{}", candidate.target_id));
        Box::pin(async { Ok(report(true)) })
    }

    fn apply(
        &self,
        request: TargetApplyRequest,
        actor: PrincipalId,
    ) -> ConfigurationFuture<'_, TargetApplyOutcome> {
        self.calls.lock().unwrap().push(format!(
            "apply:{}:{}",
            actor.as_str(),
            request.expected_digest
        ));
        let invalid = self.invalid;
        Box::pin(async move {
            if invalid {
                return Err(ConfigurationApiError::Invalid(Box::new(report(false))));
            }
            Ok(TargetApplyOutcome {
                configuration_digest: "sha256:next".to_owned(),
                running_destination: Some(destination(1)),
                next_destination: destination(2),
                restart_required: true,
                dispositions: vec![],
            })
        })
    }

    fn authorize_disposition(
        &self,
        request: TargetDispositionRequest,
        actor: PrincipalId,
    ) -> ConfigurationFuture<'_, TargetDispositionView> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("dispose:{}:{:?}", actor.as_str(), request.action));
        Box::pin(async move {
            Ok(TargetDispositionView {
                audit_event_id: "disposition-1".to_owned(),
                target_id: request.target_id,
                configuration_revision: request.configuration_revision,
                action: request.action,
                state: "authorized",
                authorized_by: actor.as_str().to_owned(),
                authorized_at: serde_json::from_str("\"2026-10-06T09:00:00Z\"").unwrap(),
            })
        })
    }
}

fn router(port: Arc<Port>) -> Router {
    let tokens = Tokens(BTreeMap::from([
        (
            "reader-token",
            grant("reader", vec![AccessPermission::ConfigurationRead], true),
        ),
        (
            "writer-token",
            grant(
                "writer",
                vec![
                    AccessPermission::ConfigurationRead,
                    AccessPermission::ConfigurationWrite,
                ],
                true,
            ),
        ),
        (
            "admin-token",
            grant(
                "admin",
                vec![
                    AccessPermission::ConfigurationWrite,
                    AccessPermission::DestructiveDisposition,
                ],
                true,
            ),
        ),
        (
            "station-token",
            grant(
                "station",
                vec![
                    AccessPermission::ConfigurationRead,
                    AccessPermission::ConfigurationWrite,
                    AccessPermission::DestructiveDisposition,
                ],
                false,
            ),
        ),
        (
            "control-token",
            grant(
                "control",
                vec![
                    AccessPermission::Read,
                    AccessPermission::Control,
                    AccessPermission::PrivilegedControl,
                ],
                true,
            ),
        ),
    ]));
    configuration_router(
        identity(),
        ManagementConfigurationSetup {
            port,
            authenticator: Arc::new(tokens),
        },
    )
}

const CANDIDATE: &str = r#"{"target_id":"main","targets":[{"id":"main","kind":"ems-scada.http","enabled":true,"settings":{"listen_addr":"127.0.0.1:9080"}}]}"#;

fn apply_body() -> String {
    format!(r#"{{"expected_digest":"sha256:digest","configuration":{CANDIDATE}}}"#)
}

fn disposition(action: &str) -> String {
    format!(r#"{{"target_id":"main","configuration_revision":1,"action":"{action}"}}"#)
}

async fn send(
    router: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<String>,
) -> (StatusCode, serde_json::Value, Option<String>) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body)),
        None => request.body(Body::empty()),
    }
    .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cache = response
        .headers()
        .get(header::CACHE_CONTROL)
        .map(|value| value.to_str().unwrap().to_owned());
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let value = if body.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    (status, value, cache)
}

#[tokio::test]
async fn every_route_requires_a_configuration_credential() {
    let port = Arc::new(Port::default());
    let router = router(port.clone());
    let endpoints = [
        ("GET", "/api/v1/configuration/targets", None),
        ("GET", "/api/v1/configuration/targets/catalog", None),
        (
            "POST",
            "/api/v1/configuration/targets/validate",
            Some(CANDIDATE.to_owned()),
        ),
        (
            "POST",
            "/api/v1/configuration/targets/apply",
            Some(apply_body()),
        ),
        (
            "POST",
            "/api/v1/configuration/targets/dispositions",
            Some(disposition("archive")),
        ),
    ];
    for (method, path, body) in endpoints {
        let (status, value, cache) = send(&router, method, path, None, body.clone()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(value["error"], "configuration.unauthenticated");
        assert_eq!(cache.as_deref(), Some("no-store"));
        let (status, _, _) = send(&router, method, path, Some("unknown"), body.clone()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        // Ordinary read/control/privileged permissions never imply configuration access, and
        // station-scoped configuration grants do not cover the bridge-wide configuration.
        for token in ["control-token", "station-token"] {
            let (status, value, _) = send(&router, method, path, Some(token), body.clone()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{token} {path}");
            assert_eq!(value["error"], "configuration.forbidden");
        }
    }
    assert!(port.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn read_permission_reads_and_validates_but_cannot_apply_or_dispose() {
    let port = Arc::new(Port::default());
    let router = router(port.clone());
    let (status, value, cache) = send(
        &router,
        "GET",
        "/api/v1/configuration/targets",
        Some("reader-token"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["configuration_digest"], "sha256:digest");
    assert_eq!(cache.as_deref(), Some("no-store"));
    let (status, _, _) = send(
        &router,
        "GET",
        "/api/v1/configuration/targets/catalog",
        Some("reader-token"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, value, _) = send(
        &router,
        "POST",
        "/api/v1/configuration/targets/validate",
        Some("reader-token"),
        Some(CANDIDATE.to_owned()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["restart_required"], true);

    for (path, body) in [
        ("/api/v1/configuration/targets/apply", apply_body()),
        (
            "/api/v1/configuration/targets/dispositions",
            disposition("archive"),
        ),
    ] {
        let (status, _, _) = send(&router, "POST", path, Some("reader-token"), Some(body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(
        *port.calls.lock().unwrap(),
        ["current", "catalog", "validate:main"]
    );
}

#[tokio::test]
async fn apply_passes_the_authenticated_actor_and_reports_restart() {
    let port = Arc::new(Port::default());
    let router = router(port.clone());
    let (status, value, _) = send(
        &router,
        "POST",
        "/api/v1/configuration/targets/apply",
        Some("writer-token"),
        Some(apply_body()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["restart_required"], true);
    assert_eq!(value["next_destination"]["configuration_revision"], 2);
    assert_eq!(*port.calls.lock().unwrap(), ["apply:writer:sha256:digest"]);
}

#[tokio::test]
async fn invalid_apply_is_unprocessable_with_the_sanitized_report() {
    let port = Arc::new(Port {
        invalid: true,
        ..Port::default()
    });
    let router = router(port);
    let (status, value, _) = send(
        &router,
        "POST",
        "/api/v1/configuration/targets/apply",
        Some("writer-token"),
        Some(apply_body()),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(value["error"], "configuration.invalid");
    assert_eq!(value["validation"]["valid"], false);
}

#[tokio::test]
async fn discard_requires_the_separate_destructive_permission() {
    let port = Arc::new(Port::default());
    let router = router(port.clone());
    let path = "/api/v1/configuration/targets/dispositions";
    let (status, value, _) = send(
        &router,
        "POST",
        path,
        Some("writer-token"),
        Some(disposition("discard")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value["error"], "configuration.forbidden");

    let (status, value, _) = send(
        &router,
        "POST",
        path,
        Some("writer-token"),
        Some(disposition("archive")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(value["authorized_by"], "writer");
    assert_eq!(value["action"], "archive");

    let (status, value, _) = send(
        &router,
        "POST",
        path,
        Some("admin-token"),
        Some(disposition("discard")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(value["action"], "discard");
    assert_eq!(
        *port.calls.lock().unwrap(),
        [
            format!("dispose:writer:{:?}", DispositionAction::Archive),
            format!("dispose:admin:{:?}", DispositionAction::Discard),
        ]
    );
}

#[tokio::test]
async fn malformed_or_unknown_request_fields_are_rejected_before_the_port() {
    let port = Arc::new(Port::default());
    let router = router(port.clone());
    for body in [
        r#"{"target_id":"main"}"#.to_owned(),
        r#"{"target_id":"main","targets":[],"revision":3}"#.to_owned(),
        r#"{"target_id":"main","targets":[{"id":"main","kind":"mqtt","settings":{"a":1.5}}]}"#
            .to_owned(),
    ] {
        let (status, value, _) = send(
            &router,
            "POST",
            "/api/v1/configuration/targets/validate",
            Some("writer-token"),
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(value["error"], "configuration.invalid_request");
    }
    let (status, _, _) = send(
        &router,
        "POST",
        "/api/v1/configuration/targets/dispositions",
        Some("admin-token"),
        Some(disposition("reroute")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(port.calls.lock().unwrap().is_empty());
}
