#![allow(clippy::result_large_err)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

static CASE_NUMBER: AtomicUsize = AtomicUsize::new(0);

#[test]
fn invalid_configuration_and_unavailable_peer_exit_with_jsonl_failures() {
    let invalid = run_case(
        "schema_version = 99\nstations = []\n",
        &scenario("ws-not-used", "expected"),
    );
    assert_eq!(invalid.status.code(), Some(2));
    assert_failure(&invalid, "unsupported_schema_version", "setup");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    drop(listener);
    let unavailable = run_case(&configuration(&endpoint), &scenario("connect", "expected"));
    assert_eq!(unavailable.status.code(), Some(2));
    assert_failure(&unavailable, "peer_unavailable", "setup");
}

#[tokio::test]
async fn failed_wire_assertion_exits_nonzero_and_stdout_remains_jsonl() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(
            tcp,
            |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
             mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                response.headers_mut().insert(
                    "Sec-WebSocket-Protocol",
                    "ocpp1.6".parse().unwrap(),
                );
                Ok(response)
            },
        )
        .await
        .unwrap();
        let call = socket.next().await.unwrap().unwrap();
        let Message::Text(call) = call else {
            panic!("expected text message");
        };
        let call: Value = serde_json::from_str(&call).unwrap();
        socket
            .send(Message::text(
                json!([3, call[1], {"currentTime": "2026-09-01T00:00:00Z"}]).to_string(),
            ))
            .await
            .unwrap();
    });

    let configuration = configuration(&endpoint);
    let scenario = scenario("heartbeat", "wrong-time");
    let output = tokio::task::spawn_blocking(move || run_case(&configuration, &scenario))
        .await
        .unwrap();
    server.await.unwrap();

    assert_eq!(output.status.code(), Some(3));
    assert_failure(&output, "unexpected_event_detail", "assertion");
}

fn run_case(configuration: &str, scenario: &str) -> Output {
    let directory = temporary_case_directory();
    let configuration_path = directory.join("simulator.toml");
    let scenario_path = directory.join("scenario.toml");
    fs::write(&configuration_path, configuration).unwrap();
    fs::write(&scenario_path, scenario).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_uob-sim"))
        .args([
            "run",
            "--config",
            configuration_path.to_str().unwrap(),
            "--scenario",
            scenario_path.to_str().unwrap(),
            "--seed",
            "77",
            "--format",
            "jsonl",
        ])
        .output()
        .unwrap();
    fs::remove_dir_all(directory).unwrap();
    output
}

fn assert_failure(output: &Output, code: &str, category: &str) {
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let events: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!events.is_empty());
    let failure = events.last().unwrap();
    assert_eq!(failure["event"], "run_failed");
    assert_eq!(failure["failure_code"], code);
    assert_eq!(failure["failure_category"], category);
    assert!(!stdout.contains("password"));
}

fn temporary_case_directory() -> PathBuf {
    let number = CASE_NUMBER.fetch_add(1, Ordering::Relaxed);
    let directory =
        std::env::temp_dir().join(format!("uob-sim-cli-{}-{number}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    directory
}

fn configuration(endpoint: &str) -> String {
    format!(
        r#"schema_version = 1
[[stations]]
id = "alpha"
endpoint = "{endpoint}"
ocpp_version = "1.6"
request_timeout_ms = 200
"#
    )
}

fn scenario(_name: &str, expected: &str) -> String {
    format!(
        r#"schema_version = 1
seed = 4
[[steps]]
id = "connect"
station = "alpha"
action = "connect"
timeout_ms = 500
[[steps]]
id = "heartbeat"
station = "alpha"
action = "heartbeat"
timeout_ms = 500
expect_message = "Heartbeat"
expect_event = "heartbeat_result"
expect_detail = "{expected}"
"#
    )
}
