use crate::control_support::{
    Fixture, HOST, TOKEN, control_document, finished, request, wait_scenario,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:39193";

fn browser_fixture() -> Fixture {
    let fixture = Fixture::new(&wait_scenario("demo-alpha", 1));
    fixture.write(
        "control.toml",
        &(control_document("demo")
            + &format!("\n[control_browser]\nconsole_origin = \"{ORIGIN}\"\n")),
    );
    fixture
}

#[test]
fn browser_origin_requires_literal_loopback_url() {
    let fixture = browser_fixture();
    for malformed in [
        "http://localhost:39193",
        "https://127.0.0.1:39193",
        "http://127.0.0.1:39193/",
        "http://user@127.0.0.1:39193",
    ] {
        fixture.write(
            "control.toml",
            &(control_document("demo")
                + &format!("\n[control_browser]\nconsole_origin = \"{malformed}\"\n")),
        );
        assert_eq!(
            fixture.load().err(),
            Some("literal_loopback_control_origin_required")
        );
    }
}

#[tokio::test]
async fn browser_requests_only_expose_cors_to_configured_origin() {
    let fixture = browser_fixture();
    let server = fixture.server();
    let router = server.router();
    let same_listener = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/scenarios")
                .header("Host", HOST)
                .header("Origin", "http://127.0.0.1:9001")
                .header("Authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(same_listener.status(), StatusCode::OK);
    assert!(
        same_listener
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
    server.shutdown().await;
}

#[tokio::test]
async fn browser_preflights_enforce_allowed_routes_methods_and_headers() {
    let fixture = browser_fixture();
    let server = fixture.server();
    assert_preflights(&server.router()).await;
    server.shutdown().await;
}

async fn assert_preflights(router: &Router) {
    for (path, method, headers, expected) in [
        (
            "/api/v1/scenarios",
            "GET",
            "authorization",
            StatusCode::NO_CONTENT,
        ),
        (
            "/api/v1/runs",
            "POST",
            "authorization,content-type",
            StatusCode::NO_CONTENT,
        ),
        (
            "/api/v1/runs/1/controls",
            "POST",
            "authorization, content-type",
            StatusCode::NO_CONTENT,
        ),
        (
            "/api/v1/scenarios",
            "DELETE",
            "authorization",
            StatusCode::FORBIDDEN,
        ),
        (
            "/api/v1/runs/1/controls",
            "GET",
            "authorization",
            StatusCode::FORBIDDEN,
        ),
        (
            "/api/v1/commands",
            "POST",
            "authorization",
            StatusCode::FORBIDDEN,
        ),
        (
            "/api/v1/runs",
            "POST",
            "authorization,cookie",
            StatusCode::FORBIDDEN,
        ),
        (
            "/api/v1/runs",
            "POST",
            "authorization,authorization",
            StatusCode::FORBIDDEN,
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri(path)
                    .header("Host", HOST)
                    .header("Origin", ORIGIN)
                    .header("Access-Control-Request-Method", method)
                    .header("Access-Control-Request-Headers", headers)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{path} {method} {headers}");
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_some(),
            expected == StatusCode::NO_CONTENT
        );
        assert!(
            response
                .headers()
                .get("access-control-allow-credentials")
                .is_none()
        );
        if expected == StatusCode::NO_CONTENT {
            let allowed = if path == "/api/v1/scenarios" {
                "GET"
            } else if path == "/api/v1/runs" {
                "GET, POST"
            } else {
                "POST"
            };
            assert_eq!(response.headers()["access-control-allow-methods"], allowed);
            assert_eq!(
                response.headers()["access-control-allow-headers"],
                "Authorization, Content-Type"
            );
        }
    }
}

#[tokio::test]
async fn browser_run_preserves_max_seed_and_catalog_metadata() {
    let fixture = browser_fixture();
    let server = fixture.server();
    let router = server.router();
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/runs")
                .header("Host", HOST)
                .header("Origin", ORIGIN)
                .header("Authorization", format!("Bearer {TOKEN}"))
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({"scenario":"sample","seed":"18446744073709551615"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["access-control-allow-origin"], ORIGIN);
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
    let started: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(started["run_id"], "1");
    let report = finished(&router, "1").await;
    assert_eq!(report["seed"], "18446744073709551615");
    assert_eq!(report["run_id"], "1");
    assert_eq!(report["events"][0]["seed"], "18446744073709551615");
    assert_eq!(
        request(&router, "GET", "/api/v1/runs", Value::Null).await.1["runs"][0]["seed"],
        "18446744073709551615"
    );
    let catalog = request(&router, "GET", "/api/v1/scenarios", Value::Null)
        .await
        .1;
    assert_eq!(catalog["scenarios"][0]["id"], "sample");
    assert_eq!(catalog["scenarios"][0]["seed"], "42");
    assert_eq!(catalog["scenarios"][0]["stations"][0], "demo-alpha");
    assert_eq!(
        catalog["scenarios"][0]["steps"][0]["eligible_controls"],
        json!(["disconnect", "reconnect"])
    );
    server.shutdown().await;
}

#[tokio::test]
async fn browser_run_rejects_noncanonical_seeds_and_unprivileged_requests() {
    let fixture = browser_fixture();
    let server = fixture.server();
    let router = server.router();
    for seed in [
        json!(18_446_744_073_709_551_615_u64),
        json!("01"),
        json!("-1"),
        json!("18446744073709551616"),
    ] {
        assert_eq!(
            request(
                &router,
                "POST",
                "/api/v1/runs",
                json!({"scenario":"sample","seed":seed})
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    for (token, seed, expected) in [
        ("read-only-token", "42", StatusCode::UNAUTHORIZED),
        (TOKEN, "01", StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/runs")
                    .header("Host", HOST)
                    .header("Origin", ORIGIN)
                    .header("Authorization", format!("Bearer {token}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        json!({"scenario":"sample","seed":seed}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        assert_eq!(response.headers()["access-control-allow-origin"], ORIGIN);
        assert!(
            response
                .headers()
                .get("access-control-allow-credentials")
                .is_none()
        );
    }
    server.shutdown().await;
}
