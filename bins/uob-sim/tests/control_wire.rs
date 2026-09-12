#![allow(clippy::result_large_err)]
mod control_support;
use control_support::{Fixture, configuration, finished, request, start};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    process::Command,
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_tungstenite::tungstenite::Message;

async fn peer(connections: usize) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        for _ in 0..connections {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(tcp, |
                request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                response.headers_mut().insert("Sec-WebSocket-Protocol", request.headers()["Sec-WebSocket-Protocol"].clone());
                Ok(response)
            }).await.unwrap();
            while let Some(message) = socket.next().await {
                let Ok(Message::Text(text)) = message else {
                    break;
                };
                let call: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(call[2], "Heartbeat");
                socket
                    .send(Message::text(
                        json!([3, call[1], {"currentTime":"2026-09-01T00:00:00Z"}]).to_string(),
                    ))
                    .await
                    .unwrap();
            }
        }
    });
    (endpoint, task)
}

fn scenario(station: &str) -> String {
    format!(
        r#"schema_version = 1
seed = 42
[[steps]]
id = "connect"
station = "{station}"
action = "connect"
timeout_ms = 1000
[[steps]]
id = "heartbeat"
station = "{station}"
action = "heartbeat"
timeout_ms = 1000
expect_message = "Heartbeat"
expect_event = "heartbeat_result"
expect_detail = "2026-09-01T00:00:00Z"
"#
    )
}

#[tokio::test]
async fn run_cli_and_serve_have_equivalent_logical_wire_results_for_both_editions() {
    for station in ["demo-alpha", "demo-beta"] {
        let (endpoint, peer) = peer(2).await;
        let fixture = Fixture::new(&scenario(station));
        fixture.write("simulator.toml", &configuration(&endpoint));
        let directory = fixture.directory.clone();
        let output = tokio::task::spawn_blocking(move || {
            Command::new(env!("CARGO_BIN_EXE_uob-sim"))
                .args(["run", "--config"])
                .arg(directory.join("simulator.toml"))
                .arg("--scenario")
                .arg(directory.join("scenario.toml"))
                .args(["--format", "jsonl"])
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let cli_events: Vec<Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let server = fixture.server();
        let router = server.router();
        let id = start(&router).await;
        let report = finished(&router, id).await;
        assert_eq!(report["status"], "passed");
        let api_events = report["events"].as_array().unwrap();
        assert_eq!(cli_events.len(), api_events.len());
        for (cli, api) in cli_events.iter().zip(api_events) {
            for key in [
                "id",
                "sequence",
                "event",
                "status",
                "seed",
                "step_id",
                "station_id",
                "action",
                "failure_category",
                "failure_code",
            ] {
                assert_eq!(cli[key], api[key], "field {key}");
            }
            assert!(api.get("detail").is_none());
        }
        assert_eq!(report["steps"][1]["assertion_passed"], true);
        server.shutdown().await;
        tokio::time::timeout(Duration::from_secs(3), peer)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn pending_controls_disconnect_reconnect_and_delay_a_real_response() {
    let (endpoint, peer) = peer(2).await;
    let scenario = scenario("demo-alpha").replace(
        "[[steps]]\nid = \"heartbeat\"",
        r#"
[[steps]]
id = "window"
station = "demo-alpha"
action = "wait"
duration_ms = 300
timeout_ms = 1000
[[steps]]
id = "disconnect-checkpoint"
station = "demo-alpha"
action = "wait"
duration_ms = 1
timeout_ms = 1000
[[steps]]
id = "reconnect-checkpoint"
station = "demo-alpha"
action = "wait"
duration_ms = 1
timeout_ms = 1000
[[steps]]
id = "heartbeat""#,
    );
    let fixture = Fixture::new(&scenario);
    fixture.write("simulator.toml", &configuration(&endpoint));
    let server = fixture.server();
    let router = server.router();
    let began = Instant::now();
    let id = start(&router).await;
    for (step, intervention) in [
        ("disconnect-checkpoint", json!({"kind":"disconnect"})),
        ("reconnect-checkpoint", json!({"kind":"reconnect"})),
        (
            "heartbeat",
            json!({"kind":"fault","fault":"response_delay","delay_ms":100}),
        ),
    ] {
        let (status, body) = request(
            &router,
            "POST",
            &format!("/api/v1/runs/{id}/controls"),
            json!({"step_id":step,"intervention":intervention}),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    }
    let report = finished(&router, id).await;
    assert_eq!(report["status"], "passed", "{report}");
    assert!(began.elapsed() >= Duration::from_millis(400));
    assert_eq!(report["steps"][2]["action"], "disconnect");
    assert_eq!(report["steps"][3]["action"], "connect");
    assert_eq!(report["steps"][4]["fault"], "response_delay");
    assert!(
        report["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["event"] == "fault_selected")
    );
    server.shutdown().await;
    tokio::time::timeout(Duration::from_secs(3), peer)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn injected_missing_response_obeys_the_existing_step_deadline() {
    let (endpoint, peer) = peer(1).await;
    let scenario = scenario("demo-alpha")
        .replace(
            "[[steps]]\nid = \"heartbeat\"",
            r#"
[[steps]]
id = "window"
station = "demo-alpha"
action = "wait"
duration_ms = 100
timeout_ms = 1000
[[steps]]
id = "heartbeat""#,
        )
        .replace(
            "timeout_ms = 1000\nexpect_message",
            "timeout_ms = 100\nexpect_message",
        );
    let fixture = Fixture::new(&scenario);
    fixture.write("simulator.toml", &configuration(&endpoint));
    let server = fixture.server();
    let router = server.router();
    let id = start(&router).await;
    assert_eq!(request(&router, "POST", &format!("/api/v1/runs/{id}/controls"), json!({
        "step_id":"heartbeat", "intervention":{"kind":"fault","fault":"missing_response","delay_ms":0}
    })).await.0, axum::http::StatusCode::OK);
    let report = finished(&router, id).await;
    assert_eq!(report["status"], "failed");
    assert_eq!(
        report["events"].as_array().unwrap().last().unwrap()["failure_code"],
        "step_timeout"
    );
    server.shutdown().await;
    tokio::time::timeout(Duration::from_secs(3), peer)
        .await
        .unwrap()
        .unwrap();
}
