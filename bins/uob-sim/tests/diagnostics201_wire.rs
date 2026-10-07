//! 2.0.1 log station behavior over a real OCPP-J socket and a loopback HTTP upload receiver.
mod diagnostics201_support;
#[allow(dead_code)]
#[path = "local_authorization201/socket.rs"]
mod socket;
use diagnostics201_support::{Received, private_directory, receiver};
use serde_json::{Value, json};
use socket::{accept, boot, receive, send};
use std::{path::Path, sync::atomic::Ordering, time::Duration};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use uob_sim::scenario::parse_configuration;
use uob_sim::{ProtocolClient, SimulatorAction, SimulatorCall, SimulatorProtocolClient};

type Socket = WebSocketStream<TcpStream>;
const LOG: &str = "LogStatusNotification";

async fn station(
    listener: &TcpListener,
    directory: &Path,
    diagnostics: Option<&str>,
) -> SimulatorProtocolClient {
    let diagnostics = diagnostics.map_or_else(String::new, |options| {
        format!(
            "[stations.diagnostics201]\nprivate_state_file='{}'\n{options}\n",
            directory.join("diagnostics.json").display()
        )
    });
    let source = format!(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}/ocpp/alpha'\nocpp_version='2.0.1'\nrequest_timeout_ms=1000\nreconnect=true\n[[stations.evses]]\nid=1\nconnectors=[1]\n{diagnostics}",
        listener.local_addr().unwrap(),
    );
    let client = SimulatorProtocolClient::connect(
        parse_configuration(&source).unwrap().stations[0].client_config(),
    )
    .await
    .unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: json!({"reason":"PowerUp","chargingStation":{"model":"Log201","vendorName":"UOB"}}),
        })
        .await
        .unwrap();
    client
}

/// Acknowledge exactly the next station CALL, which must be this log status.
async fn expect_status(socket: &mut Socket, status: &str, request_id: Option<i32>) {
    let frame = receive(socket).await;
    assert_eq!(
        (frame[0].as_u64(), frame[2].as_str()),
        (Some(2), Some(LOG)),
        "{frame}"
    );
    let mut expected = json!({ "status": status });
    if let Some(id) = request_id {
        expected["requestId"] = id.into();
    }
    assert_eq!(frame[3], expected);
    send(socket, json!([3, frame[1], {}])).await;
}

async fn call(socket: &mut Socket, id: &str, action: &str, payload: Value) -> Value {
    send(socket, json!([2, id, action, payload])).await;
    let frame = receive(socket).await;
    assert_eq!(frame[1], id, "{frame}");
    frame
}

fn get_log(log_type: &str, request_id: i32, location: &str) -> Value {
    json!({"logType": log_type, "requestId": request_id, "log": {"remoteLocation": location}})
}

async fn trigger(socket: &mut Socket, id: &str) {
    let reply = call(
        socket,
        id,
        "TriggerMessage",
        json!({"requestedMessage": LOG}),
    )
    .await;
    assert_eq!(reply[2]["status"], "Accepted");
}

#[tokio::test]
async fn both_log_types_upload_generated_logs_and_report_exact_statuses() {
    let directory = private_directory("diagnostics201-wire");
    let received = Received::default();
    let base = receiver(received.clone()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let request = json!({"logType": "DiagnosticsLog", "requestId": 4, "retries": 1,
            "log": {"remoteLocation": format!("{base}/uploads/diagnostics/"),
                    "oldestTimestamp": "2026-10-01T00:00:00Z"}});
        assert_eq!(
            call(&mut socket, "diag", "GetLog", request).await,
            json!([3, "diag", {"status": "Accepted", "filename": "diagnosticslog-alpha-1.log"}])
        );
        expect_status(&mut socket, "Uploading", Some(4)).await;
        expect_status(&mut socket, "Uploaded", Some(4)).await;
        // N01.FR.13: a triggered status with no upload ongoing has no requestId.
        trigger(&mut socket, "idle").await;
        expect_status(&mut socket, "Idle", None).await;
        let request = get_log("SecurityLog", 5, &format!("{base}/uploads/security/"));
        assert_eq!(
            call(&mut socket, "log", "GetLog", request).await,
            json!([3, "log", {"status": "Accepted", "filename": "securitylog-alpha-2.log"}])
        );
        expect_status(&mut socket, "Uploading", Some(5)).await;
        expect_status(&mut socket, "Uploaded", Some(5)).await;
        let mut unknown = get_log("SecurityLog", 6, &format!("{base}/uploads/x/"));
        unknown["unexpected"] = true.into();
        assert_eq!(
            call(&mut socket, "strict", "GetLog", unknown).await[2],
            "FormationViolation"
        );
        let invalid = get_log("SecurityLog", 7, "not a uri");
        assert_eq!(
            call(&mut socket, "uri", "GetLog", invalid).await[2],
            "PropertyConstraintViolation"
        );
    };
    let client = station(&listener, &directory, Some(""));
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(20),
        Box::pin(async { tokio::join!(peer, client) }),
    )
    .await
    .unwrap();
    let files = received.files();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].0, "diagnostics/diagnosticslog-alpha-1.log");
    assert!(files[0].1.starts_with(b"UOB-SIM DIAGNOSTICS LOG\n"));
    assert_eq!(files[0].1.len(), 4096);
    assert_eq!(files[1].0, "security/securitylog-alpha-2.log");
    assert!(files[1].1.starts_with(b"UOB-SIM SECURITY LOG\n"));
    assert_eq!(files[1].1.len(), 2048);
    let snapshot = client.diagnostics201().unwrap().snapshot();
    assert_eq!(snapshot["uploads"], 2);
    assert_eq!(snapshot["statuses"], json!(["Uploading", "Uploaded"]));
    assert!(!format!("{:?}", client.traces()).contains(&base));
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn refused_uploads_retry_then_report_the_configured_failure_status() {
    let directory = private_directory("diagnostics201-wire-failures");
    let received = Received::default();
    received.refuse.store(2, Ordering::SeqCst);
    let base = receiver(received.clone()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let mut request = get_log("DiagnosticsLog", 11, &format!("{base}/uploads/log/"));
        request["retries"] = 1.into();
        request["retryInterval"] = 0.into();
        assert_eq!(
            call(&mut socket, "log", "GetLog", request).await[2]["status"],
            "Accepted"
        );
        // N01.FR.10: the configured failure status, with the request's identity.
        for status in ["Uploading", "Uploading", "BadMessage"] {
            expect_status(&mut socket, status, Some(11)).await;
        }
    };
    let client = station(&listener, &directory, Some("failure_status='BadMessage'"));
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(20),
        Box::pin(async { tokio::join!(peer, client) }),
    )
    .await
    .unwrap();
    assert!(received.files().is_empty());
    let snapshot = client.diagnostics201().unwrap().snapshot();
    assert_eq!(
        (&snapshot["active"], &snapshot["uploads"]),
        (&json!(false), &json!(0))
    );
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn new_get_log_cancels_a_stalled_upload_with_one_accepted_canceled_for_the_old_request() {
    let directory = private_directory("diagnostics201-wire-cancel");
    let received = Received::default();
    let base = receiver(received.clone()).await;
    // Accepts connections but never answers, so the first upload stalls until its deadline.
    let stalled = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stalled_base = format!("http://{}", stalled.local_addr().unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let first = get_log(
            "SecurityLog",
            1,
            &format!("{stalled_base}/uploads/stalled/"),
        );
        assert_eq!(
            call(&mut socket, "first", "GetLog", first).await[2]["status"],
            "Accepted"
        );
        expect_status(&mut socket, "Uploading", Some(1)).await;
        // N01.FR.13: while the upload is ongoing a triggered status names its request.
        trigger(&mut socket, "ongoing").await;
        expect_status(&mut socket, "Uploading", Some(1)).await;
        let second = get_log("SecurityLog", 2, &format!("{base}/uploads/second/"));
        assert_eq!(
            call(&mut socket, "second", "GetLog", second).await,
            json!([3, "second", {"status": "AcceptedCanceled", "filename": "securitylog-alpha-2.log"}])
        );
        // N01.FR.20: the cancelled upload reports AcceptedCanceled with its own requestId.
        expect_status(&mut socket, "AcceptedCanceled", Some(1)).await;
        expect_status(&mut socket, "Uploading", Some(2)).await;
        expect_status(&mut socket, "Uploaded", Some(2)).await;
        // Outlive the stalled upload's deadline: its late failure must stay silent.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        trigger(&mut socket, "idle").await;
        expect_status(&mut socket, "Idle", None).await;
    };
    let client = station(&listener, &directory, Some("upload_timeout_ms=1000"));
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(20),
        Box::pin(async { tokio::join!(peer, client) }),
    )
    .await
    .unwrap();
    drop(stalled);
    let files = received.files();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0, "second/securitylog-alpha-2.log");
    let snapshot = client.diagnostics201().unwrap().snapshot();
    assert_eq!(snapshot["cancelled"], json!([1]));
    assert_eq!(snapshot["requestId"], 2);
    assert_eq!(snapshot["pendingStatuses"], 0);
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn station_without_a_log_model_refuses_get_log_with_not_implemented() {
    let directory = private_directory("diagnostics201-wire-none");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let request = get_log("SecurityLog", 1, "http://127.0.0.1:9/uploads/");
        assert_eq!(
            call(&mut socket, "log", "GetLog", request).await[2],
            "NotImplemented"
        );
        // Without a model a triggered status is a plain Idle (N01.FR.13).
        trigger(&mut socket, "idle").await;
        expect_status(&mut socket, "Idle", None).await;
    };
    let client = station(&listener, &directory, None);
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(20),
        Box::pin(async { tokio::join!(peer, client) }),
    )
    .await
    .unwrap();
    assert!(client.diagnostics201().is_none());
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn rejecting_station_answers_rejected_with_a_reason_and_uploads_nothing() {
    let directory = private_directory("diagnostics201-wire-reject");
    let received = Received::default();
    let base = receiver(received.clone()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let request = get_log("SecurityLog", 1, &format!("{base}/uploads/none/"));
        assert_eq!(
            call(&mut socket, "log", "GetLog", request).await,
            json!([3, "log", {"status": "Rejected", "statusInfo": {"reasonCode": "NoLogAvailable"}}])
        );
        trigger(&mut socket, "idle").await;
        expect_status(&mut socket, "Idle", None).await;
    };
    let client = station(&listener, &directory, Some("reject_get_log=true"));
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(20),
        Box::pin(async { tokio::join!(peer, client) }),
    )
    .await
    .unwrap();
    assert!(received.files().is_empty());
    assert_eq!(client.diagnostics201().unwrap().snapshot()["active"], false);
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn status_lost_with_the_socket_is_redelivered_after_the_station_registers_again() {
    let directory = private_directory("diagnostics201-wire-reconnect");
    let received = Received::default();
    let base = receiver(received.clone()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let request = get_log("SecurityLog", 21, &format!("{base}/uploads/security/"));
        assert_eq!(
            call(&mut socket, "log", "GetLog", request).await[2]["status"],
            "Accepted"
        );
        // The first status CALL is read but never acknowledged: the connection is lost.
        let lost = receive(&mut socket).await;
        assert_eq!(
            (lost[2].as_str(), &lost[3]["requestId"]),
            (Some(LOG), &json!(21))
        );
        socket.close(None).await.unwrap();
        drop(socket);
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        // At-least-once: the unacknowledged status is repeated before anything later.
        expect_status(&mut socket, "Uploading", Some(21)).await;
        expect_status(&mut socket, "Uploaded", Some(21)).await;
    };
    let client = station(&listener, &directory, Some(""));
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(30),
        Box::pin(async { tokio::join!(peer, client) }),
    )
    .await
    .unwrap();
    // The peer acknowledged the last CALL; the station records that delivery asynchronously.
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.diagnostics201().unwrap().snapshot()["lastStatus"] != "Uploaded" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        client.diagnostics201().unwrap().snapshot()["pendingStatuses"],
        0
    );
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}
