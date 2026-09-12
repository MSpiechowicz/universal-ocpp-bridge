mod control_support;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use control_support::{
    Fixture, HOST, TOKEN, control_document, finished, request, start, wait_scenario,
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uob_sim::control::ControlConfiguration;

#[test]
fn configuration_rejects_production_remote_peers_credentials_and_unbounded_work() {
    let fixture = Fixture::new(&wait_scenario("demo-alpha", 1));
    assert!(fixture.load().is_ok());
    fixture.write("control.toml", &control_document("production"));
    assert_eq!(
        fixture.load().err(),
        Some("explicit_test_environment_required")
    );
    fixture.write("control.toml", &control_document("demo"));
    assert_eq!(
        ControlConfiguration::load(
            &fixture.directory.join("control.toml"),
            "0.0.0.0:9001".parse().unwrap()
        )
        .err(),
        Some("loopback_control_bind_required")
    );
    for endpoint in [
        "ws://192.0.2.1:9000",
        "ws://localhost:9000",
        "ws://token@127.0.0.1:9000",
    ] {
        fixture.write("simulator.toml", &control_support::configuration(endpoint));
        assert!(fixture.load().is_err());
    }
    fixture.write(
        "simulator.toml",
        &control_support::configuration("ws://127.0.0.1:9000").replace("demo-", "production-"),
    );
    assert_eq!(fixture.load().err(), Some("isolated_test_peer_required"));
    fixture.write(
        "simulator.toml",
        &control_support::configuration("ws://127.0.0.1:9000"),
    );
    fixture.write("scenario.toml", &wait_scenario("demo-alpha", 30001));
    assert_eq!(fixture.load().err(), Some("scenario_control_bound"));
    fixture.write("scenario.toml", &" ".repeat(65537));
    assert_eq!(fixture.load().err(), Some("document_limit"));
    fixture.write("token", "weak");
    assert_eq!(
        fixture.load().err(),
        Some("control_token_requires_32_random_bytes_hex")
    );
}

#[tokio::test]
async fn api_requires_explicit_bearer_and_rejects_cross_origin_host_and_large_bodies() {
    let fixture = Fixture::new(&wait_scenario("demo-alpha", 1));
    let server = fixture.server();
    let router = server.router();
    for (token, host, origin, expected) in [
        ("", HOST, None, StatusCode::UNAUTHORIZED),
        ("production-token", HOST, None, StatusCode::UNAUTHORIZED),
        (TOKEN, "attacker.example", None, StatusCode::FORBIDDEN),
        (
            TOKEN,
            HOST,
            Some("https://attacker.example"),
            StatusCode::FORBIDDEN,
        ),
        (TOKEN, HOST, Some("null"), StatusCode::FORBIDDEN),
    ] {
        let mut request = Request::builder()
            .uri("/api/v1/scenarios")
            .header("Host", host)
            .header("Authorization", format!("Bearer {token}"));
        if let Some(origin) = origin {
            request = request.header("Origin", origin);
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    let (status, catalog) = request(&router, "GET", "/api/v1/scenarios", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["environment"], "demo");
    let (status, _) = request(
        &router,
        "POST",
        "/api/v1/runs",
        json!({"scenario":"x".repeat(65536)}),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let (status, _) = request(
        &router,
        "POST",
        "/api/v1/runs",
        json!({"scenario":"sample", "command":"start"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = request(
        &router,
        "POST",
        "/api/v1/commands",
        json!({"operation":"start"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    server.shutdown().await;
}

#[tokio::test]
async fn stop_is_scoped_and_busy_stations_cannot_be_started_twice() {
    let fixture = Fixture::new(&wait_scenario("demo-alpha", 30000));
    fixture.write("beta.toml", &wait_scenario("demo-beta", 100));
    fixture.write(
        "control.toml",
        &(control_document("demo") + "\n[[scenarios]]\nid = \"beta\"\npath = \"beta.toml\"\n"),
    );
    let server = fixture.server();
    let router = server.router();
    let alpha = start(&router).await;
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/v1/runs",
            json!({"scenario":"sample"})
        )
        .await
        .1["error"],
        "station_busy"
    );
    let (_, beta) = request(&router, "POST", "/api/v1/runs", json!({"scenario":"beta"})).await;
    let beta = beta["run_id"].as_u64().unwrap();
    assert_eq!(
        request(
            &router,
            "DELETE",
            &format!("/api/v1/runs/{alpha}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    request(
        &router,
        "POST",
        &format!("/api/v1/runs/{alpha}/stop"),
        json!({}),
    )
    .await;
    let alpha_report = finished(&router, alpha).await;
    assert_eq!(alpha_report["status"], "failed");
    assert_eq!(
        alpha_report["events"].as_array().unwrap().last().unwrap()["failure_category"],
        "cancelled"
    );
    assert_eq!(finished(&router, beta).await["status"], "passed");
    assert_eq!(
        request(
            &router,
            "DELETE",
            &format!("/api/v1/runs/{alpha}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    server.shutdown().await;
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/v1/runs",
            json!({"scenario":"sample"})
        )
        .await
        .1["error"],
        "server_stopping"
    );
}

#[tokio::test]
async fn retained_run_capacity_is_finite_and_delete_reclaims_it() {
    let fixture = Fixture::new(&wait_scenario("demo-alpha", 1));
    let server = fixture.server();
    let router = server.router();
    for _ in 0..8 {
        let id = start(&router).await;
        finished(&router, id).await;
    }
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/v1/runs",
            json!({"scenario":"sample"})
        )
        .await
        .1["error"],
        "run_capacity"
    );
    request(&router, "DELETE", "/api/v1/runs/1", Value::Null).await;
    let id = start(&router).await;
    assert_eq!(id, 9);
    let (_, listed) = request(&router, "GET", "/api/v1/runs", Value::Null).await;
    assert_eq!(listed["runs"].as_array().unwrap().len(), 8);
    assert_eq!(listed["runs"][7]["run_id"], 9);
    finished(&router, id).await;
    server.shutdown().await;
}

#[tokio::test]
async fn interventions_are_validated_and_late_or_repeated_edits_fail() {
    let scenario = wait_scenario("demo-alpha", 30000)
        + r#"
[[steps]]
id = "checkpoint"
station = "demo-alpha"
action = "wait"
duration_ms = 1
timeout_ms = 100
"#;
    let fixture = Fixture::new(&scenario);
    let server = fixture.server();
    let router = server.router();
    let id = start(&router).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let (_, status) =
                request(&router, "GET", &format!("/api/v1/runs/{id}"), Value::Null).await;
            if status["steps"][0]["status"] == "running" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let path = format!("/api/v1/runs/{id}/controls");
    for (step, intervention, error) in [
        ("wait", json!({"kind":"disconnect"}), "step_not_editable"),
        ("absent", json!({"kind":"disconnect"}), "unknown_step"),
        (
            "checkpoint",
            json!({"kind":"fault", "fault":"response_delay", "delay_ms":10}),
            "invalid_fault_action",
        ),
        (
            "checkpoint",
            json!({"kind":"fault", "fault":"response_delay", "delay_ms":30001}),
            "delay_limit",
        ),
    ] {
        assert_eq!(
            request(
                &router,
                "POST",
                &path,
                json!({"step_id":step,"intervention":intervention})
            )
            .await
            .1["error"],
            error
        );
    }
    let input = json!({"step_id":"checkpoint", "intervention":{"kind":"disconnect"}});
    assert_eq!(
        request(&router, "POST", &path, input.clone()).await.0,
        StatusCode::OK
    );
    assert_eq!(
        request(&router, "POST", &path, input).await.1["error"],
        "step_not_editable"
    );
    server.shutdown().await;
}
