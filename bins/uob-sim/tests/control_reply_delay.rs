mod control_support;

use control_support::{Fixture, configuration, finished, request, start};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::{net::TcpListener, sync::oneshot};
use tokio_tungstenite::tungstenite::{
    Message,
    handshake::server::{ErrorResponse, Request, Response},
};

#[tokio::test]
async fn browser_delay_holds_a_single_real_remote_reply_on_each_ocpp_socket() {
    for (station, action, payload, assertion_failure) in [
        (
            "demo-alpha",
            "RemoteStartTransaction",
            json!({"idTag":"LOCAL-USER-1"}),
            false,
        ),
        (
            "demo-beta",
            "RequestStartTransaction",
            json!({
                "remoteStartId":71,"idToken":{"idToken":"REMOTE-USER","type":"Central"}
            }),
            false,
        ),
        (
            "demo-alpha",
            "RemoteStartTransaction",
            json!({"idTag":"LOCAL-USER-1"}),
            true,
        ),
    ] {
        exercise_delayed_remote_reply(station, action, payload, assertion_failure).await;
    }
}

async fn exercise_delayed_remote_reply(
    station: &str,
    action: &'static str,
    payload: Value,
    assertion_failure: bool,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let (trigger, release) = oneshot::channel::<()>();
    let peer = tokio::spawn(observe_remote_reply(listener, release, action, payload));
    let fixture = Fixture::new(&remote_scenario(station, assertion_failure));
    fixture.write("simulator.toml", &configuration(&endpoint));
    let server = fixture.server();
    let router = server.router();
    let (_, catalog) = request(&router, "GET", "/api/v1/scenarios", Value::Null).await;
    assert_eq!(catalog["scenarios"][0]["seed"], "18446744073709551615");
    assert_eq!(
        catalog["scenarios"][0]["steps"][2]["response_delay_scope"],
        "peer_reply"
    );
    assert!(
        catalog["scenarios"][0]["steps"][2]["eligible_controls"]
            .as_array()
            .unwrap()
            .contains(&json!("response_delay"))
    );

    let id = start(&router).await;
    let (code, scheduled) = request(
        &router,
        "POST",
        &format!("/api/v1/runs/{id}/controls"),
        json!({"step_id":"remote","intervention":{"kind":"fault","fault":"response_delay","delay_ms":150}}),
    )
    .await;
    assert_eq!(code, axum::http::StatusCode::OK, "{scheduled}");
    assert_eq!(scheduled["status"], "scheduled");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let (_, status) =
                request(&router, "GET", &format!("/api/v1/runs/{id}"), Value::Null).await;
            if status["steps"][2]["status"] == "running" {
                assert_eq!(status["steps"][2]["effect_status"], "in_progress");
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    trigger.send(()).unwrap();
    tokio::time::sleep(Duration::from_millis(35)).await;
    let (_, during) = request(&router, "GET", &format!("/api/v1/runs/{id}"), Value::Null).await;
    assert_ne!(during["steps"][2]["status"], "passed");

    let report = finished(&router, &id).await;
    assert_delayed_reply_report(&report, assertion_failure);
    server.shutdown().await;
    let elapsed = tokio::time::timeout(Duration::from_secs(3), peer)
        .await
        .unwrap()
        .unwrap();
    assert!(
        elapsed >= Duration::from_millis(125),
        "peer observed {elapsed:?}"
    );
}

async fn observe_remote_reply(
    listener: TcpListener,
    release: oneshot::Receiver<()>,
    action: &str,
    payload: Value,
) -> Duration {
    let (tcp, _) = listener.accept().await.unwrap();
    let mut socket = tokio_tungstenite::accept_hdr_async(tcp, select_protocol)
        .await
        .unwrap();
    release.await.unwrap();
    let sent = Instant::now();
    socket
        .send(Message::text(
            json!([2, "controlled-command", action, payload]).to_string(),
        ))
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Text(text) = response else {
        panic!("expected OCPP reply");
    };
    let frame: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(frame[0], 3);
    assert_eq!(frame[1], "controlled-command");
    assert_eq!(frame[2]["status"], "Accepted");
    let elapsed = sent.elapsed();
    while let Some(message) = socket.next().await {
        if message.is_err() {
            break;
        }
    }
    elapsed
}

// Tungstenite's callback requires Result even when this test always accepts the handshake.
#[allow(clippy::result_large_err, clippy::unnecessary_wraps)]
fn select_protocol(request: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
    response.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        request.headers()["Sec-WebSocket-Protocol"].clone(),
    );
    Ok(response)
}

fn remote_scenario(station: &str, assertion_failure: bool) -> String {
    let assertion = if assertion_failure {
        "expect_detail = \"wrong\"\n"
    } else {
        ""
    };
    format!(
        r#"schema_version = 1
seed = "18446744073709551615"
[[steps]]
id = "connect"
station = "{station}"
action = "connect"
timeout_ms = 1000
[[steps]]
id = "window"
station = "{station}"
action = "wait"
duration_ms = 200
timeout_ms = 1000
[[steps]]
id = "remote"
station = "{station}"
action = "await_remote_start"
timeout_ms = 1500
{assertion}"#
    )
}

fn assert_delayed_reply_report(report: &Value, assertion_failure: bool) {
    assert_eq!(
        report["status"],
        if assertion_failure {
            "failed"
        } else {
            "passed"
        },
        "{report}"
    );
    assert_eq!(report["seed"], "18446744073709551615");
    assert_eq!(report["steps"][2]["effect_status"], "applied");
    assert_eq!(report["steps"][2]["actual_event"], "remote_start_received");
    if assertion_failure {
        assert_eq!(
            report["steps"][2]["failure_code"],
            "unexpected_event_detail"
        );
        assert_eq!(report["failure"]["category"], "assertion");
        assert_eq!(report["failure"]["code"], "unexpected_event_detail");
    }
}
