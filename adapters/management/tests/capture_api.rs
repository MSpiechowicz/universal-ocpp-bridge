use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use std::sync::Arc;
use tower::ServiceExt;
use uob_application::{
    Application,
    capture::{CaptureFilter, CaptureGrant, CaptureManager, CapturePermission},
};
use uob_contracts::{
    ArtifactDigest, BridgeId, Environment, ProcessInstanceId, ReleaseId, RuntimeIdentity,
    ServiceIdentity, StationId,
};
use uob_management_adapter::{ManagementCaptureAuthenticator, ManagementCaptureConfiguration};

struct Auth;
impl ManagementCaptureAuthenticator for Auth {
    fn authenticate(&self, token: &str) -> Option<CaptureGrant> {
        let (permission, station) = match token {
            "capture-a" => (CapturePermission::Capture, "a"),
            "read-a" => (CapturePermission::Read, "a"),
            "read-b" => (CapturePermission::Read, "b"),
            _ => return None,
        };
        Some(
            CaptureGrant::new(
                BridgeId::new("bridge").unwrap(),
                vec![permission],
                Some(vec![StationId::new(station).unwrap()]),
                None,
            )
            .unwrap(),
        )
    }
}
fn identity() -> ServiceIdentity {
    ServiceIdentity {
        bridge_id: BridgeId::new("bridge").unwrap(),
        runtime: RuntimeIdentity {
            environment: Environment::Production,
            release_id: ReleaseId::new("release").unwrap(),
            release_digest: ArtifactDigest::new("sha256:test").unwrap(),
            process_instance_id: ProcessInstanceId::new("process").unwrap(),
        },
        selected_target_id: None,
    }
}
fn router(enabled: bool) -> (axum::Router, CaptureManager) {
    let manager = CaptureManager::new(enabled);
    let router = uob_management_adapter::router(Application::new(identity())).merge(
        uob_management_adapter::capture_router(
            identity(),
            ManagementCaptureConfiguration {
                manager: manager.clone(),
                authenticator: Arc::new(Auth),
            },
        ),
    );
    (router, manager)
}
async fn call(
    router: &axum::Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> axum::response::Response {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    router
        .clone()
        .oneshot(request.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap()
}
const ROOT: &str = "/api/v1/diagnostics/capture";
#[tokio::test]
async fn monitoring_never_starts_capture_and_disabled_or_unauthorized_start_fails() {
    let (router, manager) = router(false);
    for path in ["/", "/api/v1/identity"] {
        assert_eq!(
            call(&router, "GET", path, None, "").await.status(),
            StatusCode::OK
        );
    }
    let _ = call(&router, "GET", "/health", None, "").await;
    assert!(
        manager
            .status(&Auth.authenticate("read-a").unwrap())
            .is_err()
    );
    assert_eq!(
        call(&router, "POST", ROOT, None, r#"{"station_id":"a"}"#)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &router,
            "POST",
            ROOT,
            Some("capture-a"),
            r#"{"station_id":"a"}"#
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn authenticated_routes_preserve_selection_and_separate_read_from_capture() {
    let (router, manager) = router(true);
    assert_eq!(
        call(
            &router,
            "POST",
            ROOT,
            Some("read-a"),
            r#"{"station_id":"a"}"#
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let response = call(
        &router,
        "POST",
        ROOT,
        Some("capture-a"),
        r#"{"station_id":"a","level":"redacted_payload"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["identity"]["runtime"]["environment"], "production");
    let id = body["id"].as_u64().unwrap();
    assert_eq!(
        call(
            &router,
            "POST",
            ROOT,
            Some("capture-a"),
            r#"{"station_id":"a"}"#
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&router, "GET", ROOT, Some("read-b"), "")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&router, "GET", ROOT, Some("capture-a"), "")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&router, "GET", ROOT, Some("read-a"), "")
            .await
            .status(),
        StatusCode::OK
    );
    for export in [false, true] {
        assert!(
            manager
                .lease(&Auth.authenticate("read-b").unwrap(), id, export)
                .is_err()
        );
    }
}

#[tokio::test]
async fn explicit_extensions_and_stops_require_capture_permission() {
    let (router, _) = router(true);
    assert_eq!(
        call(
            &router,
            "POST",
            ROOT,
            Some("capture-a"),
            r#"{"station_id":"a"}"#
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    let id = 1;
    assert_eq!(
        call(
            &router,
            "POST",
            &format!("{ROOT}/old-process/{id}/stop"),
            Some("capture-a"),
            ""
        )
        .await
        .status(),
        StatusCode::GONE
    );
    let extend = format!("{ROOT}/process/{id}/extend");
    assert_eq!(
        call(
            &router,
            "POST",
            &extend,
            Some("read-a"),
            r#"{"duration_seconds":30}"#
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &router,
            "POST",
            &extend,
            Some("capture-a"),
            r#"{"duration_seconds":1801}"#
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &router,
            "POST",
            &extend,
            Some("capture-a"),
            r#"{"duration_seconds":30}"#
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(
            &router,
            "POST",
            &format!("{ROOT}/process/{id}/stop"),
            Some("capture-a"),
            ""
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&router, "GET", ROOT, Some("read-a"), "")
            .await
            .status(),
        StatusCode::GONE
    );
}
#[tokio::test]
async fn disconnected_browser_does_not_preserve_capture_or_allow_stale_extension() {
    let (router, manager) = router(true);
    let response = call(
        &router,
        "POST",
        ROOT,
        Some("capture-a"),
        r#"{"station_id":"a","duration_seconds":1}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    drop(response);
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    assert!(!manager.accepts(
        &CaptureFilter {
            bridge: identity().bridge_id,
            station: Some(StationId::new("a").unwrap()),
            target: None
        },
        uob_application::capture::CaptureLevel::Metadata
    ));
    assert_eq!(
        call(
            &router,
            "POST",
            &format!("{ROOT}/process/1/extend"),
            Some("capture-a"),
            r#"{"duration_seconds":30}"#
        )
        .await
        .status(),
        StatusCode::GONE
    );
}
