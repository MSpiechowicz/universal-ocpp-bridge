mod control_support;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use control_support::{Fixture, HOST, TOKEN, control_document, finished, start, wait_scenario};
use serde_json::Value;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:39193";
const READ: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn enable(fixture: &Fixture, origin: &str) {
    fixture.write("read-token", READ);
    fixture.write(
        "control.toml",
        &(control_document("demo")
            + &format!("\n[debug]\nconsole_origin = \"{origin}\"\ntoken_file = \"read-token\"\n")),
    );
}
async fn browser(
    router: &Router,
    method: &str,
    path: &str,
    origin: &str,
    token: &str,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("Host", HOST)
                .header("Origin", origin)
                .header("Authorization", format!("Bearer {token}"))
                .header("Access-Control-Request-Method", "GET")
                .header("Access-Control-Request-Headers", "authorization")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[test]
fn debug_requires_literal_origin_and_separate_secret() {
    let fixture = Fixture::new(&wait_scenario("demo-alpha", 1));
    for origin in [
        "https://example.com",
        "http://localhost:8080",
        "http://127.0.0.1:8080/",
        "http://secret@127.0.0.1:8080",
        "http://127.0.0.1:8080?secret",
    ] {
        enable(&fixture, origin);
        assert!(fixture.load().is_err(), "{origin}");
    }
    enable(&fixture, ORIGIN);
    assert!(fixture.load().is_ok());
    fixture.write("read-token", TOKEN);
    assert_eq!(fixture.load().err(), Some("separate_debug_token_required"));
}

#[tokio::test]
async fn debug_is_opt_in_get_only_origin_bound_and_cannot_use_control_credentials() {
    let fixture = Fixture::new(&wait_scenario("demo-alpha", 1));
    let disabled = fixture.server();
    assert_eq!(
        browser(
            &disabled.router(),
            "GET",
            "/api/v1/debug/runs/1",
            ORIGIN,
            READ
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    enable(&fixture, ORIGIN);
    let server = fixture.server();
    let router = server.router();
    let id = start(&router).await;
    finished(&router, id).await;
    for (method, path, origin, token, expected) in [
        (
            "GET",
            "/api/v1/debug/runs/1",
            ORIGIN,
            TOKEN,
            StatusCode::UNAUTHORIZED,
        ),
        (
            "GET",
            "/api/v1/debug/runs/1",
            "http://127.0.0.1:39189",
            READ,
            StatusCode::FORBIDDEN,
        ),
        (
            "GET",
            "/api/v1/debug/runs/1",
            "null",
            READ,
            StatusCode::FORBIDDEN,
        ),
        (
            "POST",
            "/api/v1/debug/runs/1",
            ORIGIN,
            READ,
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            "DELETE",
            "/api/v1/debug/runs/1",
            ORIGIN,
            READ,
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            "POST",
            "/api/v1/runs/1/controls",
            ORIGIN,
            READ,
            StatusCode::FORBIDDEN,
        ),
        (
            "POST",
            "/api/v1/runs",
            "http://127.0.0.1:9001",
            READ,
            StatusCode::UNAUTHORIZED,
        ),
    ] {
        assert_eq!(
            browser(&router, method, path, origin, token).await.status(),
            expected
        );
    }
    let preflight = browser(&router, "OPTIONS", "/api/v1/debug/runs/1", ORIGIN, "").await;
    assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
    assert_eq!(preflight.headers()["access-control-allow-methods"], "GET");
    assert!(
        preflight
            .headers()
            .get("access-control-allow-credentials")
            .is_none()
    );
    server.shutdown().await;
}

#[tokio::test]
async fn failed_run_preserves_actual_event_exact_seed_and_safe_failure_without_wire_details() {
    let fixture = Fixture::new(
        &(wait_scenario("demo-alpha", 1).replace("seed = 42", "seed = 18446744073709551615")
            + "expect_detail = \"private-expected-value\"\n"),
    );
    enable(&fixture, ORIGIN);
    let server = fixture.server();
    let router = server.router();
    let id = start(&router).await;
    finished(&router, id).await;
    let response = browser(&router, "GET", "/api/v1/debug/runs/1", ORIGIN, READ).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["access-control-allow-origin"], ORIGIN);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(!text.contains("private-expected-value"));
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["seed"], "18446744073709551615");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["status"], "failed");
    let step = &value["steps"][0];
    assert_eq!(step["expectation"], "delay_elapsed");
    assert_eq!(step["actual_event"], "delay_elapsed");
    assert_eq!(step["assertion_passed"], false);
    assert_eq!(step["failure_code"], "unexpected_event_detail");
    assert_eq!(step["detail_assertion"], true);
    assert!(step["correlation_id"].is_null());
    server.shutdown().await;
}
