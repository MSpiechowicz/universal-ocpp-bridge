//! Diagnostics and log-upload station behavior over a real OCPP-J socket and a loopback HTTP
//! upload receiver.
mod diagnostics16_support;
#[allow(dead_code)]
#[path = "local_authorization16/socket.rs"]
mod socket;
use diagnostics16_support::{Received, private_directory, receiver};
use serde_json::{Value, json};
use socket::{accept, boot, receive, send};
use std::{path::Path, sync::atomic::Ordering, time::Duration};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use uob_sim::scenario::parse_configuration;
use uob_sim::{ProtocolClient, SimulatorAction, SimulatorCall, SimulatorProtocolClient};

type Socket = WebSocketStream<TcpStream>;
const LEGACY: &str = "DiagnosticsStatusNotification";
const LOG: &str = "LogStatusNotification";

async fn station(
    listener: &TcpListener,
    directory: &Path,
    diagnostics: &str,
) -> SimulatorProtocolClient {
    let source = format!(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}/ocpp/alpha'\nocpp_version='1.6'\n[stations.diagnostics16]\nprivate_state_file='{}'\n{diagnostics}\n",
        listener.local_addr().unwrap(),
        directory.join("diagnostics.json").display()
    );
    let client = SimulatorProtocolClient::connect(
        parse_configuration(&source).unwrap().stations[0].client_config(),
    )
    .await
    .unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: json!({"chargePointVendor":"UOB","chargePointModel":"Diagnostics"}),
        })
        .await
        .unwrap();
    client
}

/// Acknowledge exactly the next station CALL, which must be this upload status.
async fn expect_status(socket: &mut Socket, action: &str, status: &str, request_id: Option<i32>) {
    let frame = receive(socket).await;
    assert_eq!(
        (frame[0].as_u64(), frame[2].as_str()),
        (Some(2), Some(action)),
        "{frame}"
    );
    let mut expected = json!({"status": status});
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

#[tokio::test]
async fn both_families_upload_generated_logs_and_report_exact_statuses() {
    let directory = private_directory("diagnostics-wire");
    let received = Received::default();
    let base = receiver(received.clone()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let request = json!({"location": format!("{base}/uploads/legacy/"), "startTime": "2026-10-01T00:00:00Z"});
        assert_eq!(
            call(&mut socket, "diag", "GetDiagnostics", request).await,
            json!([3, "diag", {"fileName": "diagnostics-alpha-1.log"}])
        );
        expect_status(&mut socket, LEGACY, "Uploading", None).await;
        expect_status(&mut socket, LEGACY, "Uploaded", None).await;
        // §4.4: a triggered status after the upload reports Idle.
        let trigger = call(
            &mut socket,
            "trigger",
            "TriggerMessage",
            json!({"requestedMessage":"DiagnosticsStatusNotification"}),
        )
        .await;
        assert_eq!(trigger[2]["status"], "Accepted");
        expect_status(&mut socket, LEGACY, "Idle", None).await;
        let request = json!({"logType": "SecurityLog", "requestId": 5,
            "log": {"remoteLocation": format!("{base}/uploads/security/")}});
        assert_eq!(
            call(&mut socket, "log", "GetLog", request).await,
            json!([3, "log", {"status": "Accepted", "filename": "securitylog-alpha-2.log"}])
        );
        expect_status(&mut socket, LOG, "Uploading", Some(5)).await;
        expect_status(&mut socket, LOG, "Uploaded", Some(5)).await;
        let unknown = json!({"logType": "SecurityLog", "requestId": 6,
            "log": {"remoteLocation": format!("{base}/uploads/x/")}, "unexpected": true});
        assert_eq!(
            call(&mut socket, "strict", "GetLog", unknown).await[2],
            "FormationViolation"
        );
        let invalid = json!({"location": "not a uri"});
        assert_eq!(
            call(&mut socket, "uri", "GetDiagnostics", invalid).await[2],
            "PropertyConstraintViolation"
        );
    };
    let client = station(&listener, &directory, "legacy=true\nsecurity_log=true");
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(20),
        Box::pin(async { tokio::join!(peer, client) }),
    )
    .await
    .unwrap();
    let files = received.files();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].0, "legacy/diagnostics-alpha-1.log");
    assert!(files[0].1.starts_with(b"UOB-SIM DIAGNOSTICS LOG\n"));
    assert_eq!(files[0].1.len(), 4096);
    assert_eq!(files[1].0, "security/securitylog-alpha-2.log");
    assert!(files[1].1.starts_with(b"UOB-SIM SECURITY LOG\n"));
    assert_eq!(files[1].1.len(), 2048);
    let snapshot = client.diagnostics16().unwrap().snapshot();
    assert_eq!(snapshot["uploads"], 2);
    assert_eq!(snapshot["statuses"], json!(["Uploading", "Uploaded"]));
    assert!(!format!("{:?}", client.traces()).contains(&base));
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn refused_uploads_retry_then_report_the_family_failure_status() {
    let directory = private_directory("diagnostics-wire-failures");
    let received = Received::default();
    received.refuse.store(3, Ordering::SeqCst);
    let base = receiver(received.clone()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let request = json!({"location": format!("{base}/uploads/legacy/"), "retries": 1, "retryInterval": 0});
        assert_eq!(
            call(&mut socket, "diag", "GetDiagnostics", request).await[2]["fileName"],
            "diagnostics-alpha-1.log"
        );
        for status in ["Uploading", "Uploading", "UploadFailed"] {
            expect_status(&mut socket, LEGACY, status, None).await;
        }
        let request = json!({"logType": "DiagnosticsLog", "requestId": 11,
            "log": {"remoteLocation": format!("{base}/uploads/log/")}});
        assert_eq!(
            call(&mut socket, "log", "GetLog", request).await[2]["status"],
            "Accepted"
        );
        // N01.FR.10: the configured failure status, with the request's identity.
        for status in ["Uploading", "BadMessage"] {
            expect_status(&mut socket, LOG, status, Some(11)).await;
        }
    };
    let client = station(
        &listener,
        &directory,
        "security_log=true\nget_log_failure_status='BadMessage'",
    );
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(20),
        Box::pin(async { tokio::join!(peer, client) }),
    )
    .await
    .unwrap();
    assert!(received.files().is_empty());
    let snapshot = client.diagnostics16().unwrap().snapshot();
    assert_eq!(
        (&snapshot["active"], &snapshot["uploads"]),
        (&json!(false), &json!(0))
    );
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn new_get_log_cancels_a_stalled_upload_which_then_reports_nothing() {
    let directory = private_directory("diagnostics-wire-cancel");
    let received = Received::default();
    let base = receiver(received.clone()).await;
    // Accepts connections but never answers, so the first upload stalls until its deadline.
    let stalled = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stalled_base = format!("http://{}", stalled.local_addr().unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let first = json!({"logType": "SecurityLog", "requestId": 1,
            "log": {"remoteLocation": format!("{stalled_base}/uploads/stalled/")}});
        assert_eq!(
            call(&mut socket, "first", "GetLog", first).await[2]["status"],
            "Accepted"
        );
        expect_status(&mut socket, LOG, "Uploading", Some(1)).await;
        let second = json!({"logType": "SecurityLog", "requestId": 2,
            "log": {"remoteLocation": format!("{base}/uploads/second/")}});
        assert_eq!(
            call(&mut socket, "second", "GetLog", second).await,
            json!([3, "second", {"status": "AcceptedCanceled", "filename": "securitylog-alpha-2.log"}])
        );
        expect_status(&mut socket, LOG, "Uploading", Some(2)).await;
        expect_status(&mut socket, LOG, "Uploaded", Some(2)).await;
        // Outlive the stalled upload's deadline: its late failure must stay silent.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let trigger = call(
            &mut socket,
            "trigger",
            "TriggerMessage",
            json!({"requestedMessage":"DiagnosticsStatusNotification"}),
        )
        .await;
        assert_eq!(trigger[2]["status"], "Accepted");
        expect_status(&mut socket, LEGACY, "Idle", None).await;
    };
    let client = station(
        &listener,
        &directory,
        "security_log=true\nupload_timeout_ms=1000",
    );
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
    let snapshot = client.diagnostics16().unwrap().snapshot();
    assert_eq!(snapshot["cancelled"], json!([1]));
    assert_eq!(snapshot["requestId"], 2);
    assert_eq!(snapshot["pendingStatuses"], 0);
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn legacy_only_station_refuses_get_log_with_not_implemented() {
    let directory = private_directory("diagnostics-wire-legacy-only");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let request = json!({"logType": "SecurityLog", "requestId": 1,
            "log": {"remoteLocation": "http://127.0.0.1:9/uploads/"}});
        assert_eq!(
            call(&mut socket, "log", "GetLog", request).await[2],
            "NotImplemented"
        );
    };
    let client = station(&listener, &directory, "no_diagnostics=true");
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(20),
        Box::pin(async { tokio::join!(peer, client) }),
    )
    .await
    .unwrap();
    assert_eq!(client.diagnostics16().unwrap().snapshot()["active"], false);
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}
