//! Firmware station behavior over a real OCPP-J socket and a local HTTP artifact server.
mod firmware16_support;
#[allow(dead_code)]
#[path = "local_authorization16/socket.rs"]
mod socket;
use axum::{Router, http::StatusCode, routing::get};
use firmware16_support::{TestPki, private_directory, timestamp};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use socket::{accept, boot, closed, receive, send};
use std::{fmt::Write as _, path::Path, time::Duration};
use time::OffsetDateTime;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use uob_sim::scenario::parse_configuration;
use uob_sim::{ProtocolClient, SimulatorAction, SimulatorCall, SimulatorProtocolClient};

type Socket = WebSocketStream<TcpStream>;
const IMAGE: &[u8] = b"independent wire firmware image 122";

async fn artifacts(image: &'static [u8]) -> String {
    let router = Router::new()
        .route("/firmware.bin", get(move || async move { image }))
        .route("/missing.bin", get(|| async { StatusCode::NOT_FOUND }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    base
}

async fn station(
    listener: &TcpListener,
    directory: &Path,
    firmware: &str,
) -> SimulatorProtocolClient {
    let source = format!(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}/ocpp/alpha'\nocpp_version='1.6'\n[stations.firmware16]\nprivate_state_file='{}'\n{firmware}\n",
        listener.local_addr().unwrap(),
        directory.join("firmware.json").display()
    );
    SimulatorProtocolClient::connect(
        parse_configuration(&source).unwrap().stations[0].client_config(),
    )
    .await
    .unwrap()
}

async fn register(client: &SimulatorProtocolClient) {
    client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: json!({"chargePointVendor":"UOB","chargePointModel":"Firmware"}),
        })
        .await
        .unwrap();
}

/// Acknowledge exactly the next station CALL, which must be this firmware status.
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
    assert_eq!(frame[1], id);
    frame
}

/// The rebooted station registers again carrying the installed image's version.
async fn rebooted(listener: &TcpListener, socket: &mut Socket, image: &[u8]) -> Socket {
    closed(socket).await;
    let mut socket = accept(listener).await;
    let request = receive(&mut socket).await;
    assert_eq!(request[2], "BootNotification");
    let digest = Sha256::digest(image);
    let version = digest[..6]
        .iter()
        .fold("sha256-".to_owned(), |mut version, byte| {
            write!(version, "{byte:02x}").unwrap();
            version
        });
    assert_eq!(request[3]["firmwareVersion"], version);
    send(&mut socket, json!([3,request[1],{"status":"Accepted","currentTime":"2026-10-06T00:00:00Z","interval":60}])).await;
    socket
}

fn signed_request(
    pki: &TestPki,
    id: i32,
    location: &str,
    retrieve: OffsetDateTime,
    image: &[u8],
) -> Value {
    json!({"requestId": id, "firmware": {
        "location": location, "retrieveDateTime": timestamp(retrieve),
        "signingCertificate": pki.certificate_pem, "signature": pki.sign(image)}})
}

#[tokio::test]
async fn legacy_station_downloads_installs_reboots_and_reports_installed() {
    let directory = private_directory("firmware-wire-legacy");
    let base = artifacts(IMAGE).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let payload = json!({"location": format!("{base}/firmware.bin"), "retrieveDate": "2020-01-01T00:00:00Z", "retries": 1});
        assert_eq!(
            call(&mut socket, "update", "UpdateFirmware", payload).await,
            json!([3, "update", {}])
        );
        for status in ["Downloading", "Downloaded", "Installing"] {
            expect_status(&mut socket, "FirmwareStatusNotification", status, None).await;
        }
        let mut socket = rebooted(&listener, &mut socket, IMAGE).await;
        expect_status(&mut socket, "FirmwareStatusNotification", "Installed", None).await;
        let trigger = call(
            &mut socket,
            "trigger",
            "TriggerMessage",
            json!({"requestedMessage":"FirmwareStatusNotification"}),
        )
        .await;
        assert_eq!(trigger[2]["status"], "Accepted");
        expect_status(&mut socket, "FirmwareStatusNotification", "Idle", None).await;
        let signed = json!({"requestId": 1, "firmware": {"location": "http://x/", "retrieveDateTime": "2020-01-01T00:00:00Z", "signingCertificate": "x", "signature": "x"}});
        assert_eq!(
            call(&mut socket, "signed", "SignedUpdateFirmware", signed).await[2],
            "NotImplemented"
        );
        let unknown = json!({"location": format!("{base}/firmware.bin"), "retrieveDate": "2020-01-01T00:00:00Z", "unexpected": true});
        assert_eq!(
            call(&mut socket, "strict", "UpdateFirmware", unknown).await[2],
            "FormationViolation"
        );
    };
    let client = async {
        let client = station(&listener, &directory, "mode='legacy'").await;
        register(&client).await;
        client
    };
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(20),
        Box::pin(async { tokio::join!(peer, client) }),
    )
    .await
    .unwrap();
    let snapshot = client.firmware16().unwrap().snapshot();
    assert_eq!(
        snapshot["statuses"],
        json!(["Downloading", "Downloaded", "Installing", "Installed"])
    );
    assert_eq!(snapshot["reboots"], 1);
    assert!(!format!("{:?}", client.traces()).contains(&base));
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn signed_station_validates_certificate_signature_schedule_and_cancellation() {
    let directory = private_directory("firmware-wire-signed");
    let pki = TestPki::generate();
    let stranger = TestPki::generate();
    std::fs::write(directory.join("root.pem"), &pki.root_pem).unwrap();
    let base = artifacts(IMAGE).await;
    let location = format!("{base}/firmware.bin");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let signed = "SignedFirmwareStatusNotification";
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let legacy = json!({"location": location, "retrieveDate": "2020-01-01T00:00:00Z"});
        assert_eq!(
            call(&mut socket, "legacy", "UpdateFirmware", legacy).await[2],
            "NotSupported"
        );
        let now = OffsetDateTime::now_utc();
        let untrusted = signed_request(&stranger, 40, &location, now, IMAGE);
        assert_eq!(
            call(&mut socket, "untrusted", "SignedUpdateFirmware", untrusted).await[2]["status"],
            "InvalidCertificate"
        );
        let tampered = signed_request(&pki, 41, &location, now, b"another image");
        assert_eq!(
            call(&mut socket, "tampered", "SignedUpdateFirmware", tampered).await[2]["status"],
            "Accepted"
        );
        for status in ["Downloading", "Downloaded", "InvalidSignature"] {
            expect_status(&mut socket, signed, status, Some(41)).await;
        }
        let scheduled =
            signed_request(&pki, 42, &location, now + time::Duration::seconds(1), IMAGE);
        assert_eq!(
            call(&mut socket, "scheduled", "SignedUpdateFirmware", scheduled).await[2]["status"],
            "Accepted"
        );
        for status in [
            "DownloadScheduled",
            "Downloading",
            "Downloaded",
            "SignatureVerified",
            "Installing",
            "InstallRebooting",
        ] {
            expect_status(&mut socket, signed, status, Some(42)).await;
        }
        let mut socket = rebooted(&listener, &mut socket, IMAGE).await;
        expect_status(&mut socket, signed, "Installed", Some(42)).await;
        let later = OffsetDateTime::now_utc() + time::Duration::hours(1);
        let first = signed_request(&pki, 43, &location, later, IMAGE);
        assert_eq!(
            call(&mut socket, "first", "SignedUpdateFirmware", first).await[2]["status"],
            "Accepted"
        );
        expect_status(&mut socket, signed, "DownloadScheduled", Some(43)).await;
        let second = signed_request(&pki, 44, &location, later, IMAGE);
        assert_eq!(
            call(&mut socket, "second", "SignedUpdateFirmware", second).await[2]["status"],
            "AcceptedCanceled"
        );
        expect_status(&mut socket, signed, "DownloadScheduled", Some(44)).await;
    };
    let config = format!(
        "mode='signed'\nmanufacturer_root_file='{}'",
        directory.join("root.pem").display()
    );
    let client = async {
        let client = station(&listener, &directory, &config).await;
        register(&client).await;
        client
    };
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(30),
        Box::pin(async { tokio::join!(peer, client) }),
    )
    .await
    .unwrap();
    let snapshot = client.firmware16().unwrap().snapshot();
    assert_eq!(snapshot["requestId"], 44);
    assert_eq!(snapshot["cancelled"], json!([43]));
    assert!(
        snapshot["installedVersion"]
            .as_str()
            .unwrap()
            .starts_with("sha256-")
    );
    let traces = format!("{:?}", client.traces());
    assert!(!traces.contains(&base) && !traces.contains("BEGIN CERTIFICATE"));
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn bounded_download_failures_retry_then_report_download_failed() {
    let directory = private_directory("firmware-wire-failures");
    let base = artifacts(IMAGE).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let legacy = "FirmwareStatusNotification";
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let oversized = json!({"location": format!("{base}/firmware.bin"), "retrieveDate": "2020-01-01T00:00:00Z", "retries": 1, "retryInterval": 0});
        assert_eq!(
            call(&mut socket, "oversized", "UpdateFirmware", oversized).await[2],
            json!({})
        );
        for status in ["Downloading", "Downloading", "DownloadFailed"] {
            expect_status(&mut socket, legacy, status, None).await;
        }
        let missing = json!({"location": format!("{base}/missing.bin"), "retrieveDate": "2020-01-01T00:00:00Z"});
        assert_eq!(
            call(&mut socket, "missing", "UpdateFirmware", missing).await[2],
            json!({})
        );
        for status in ["Downloading", "DownloadFailed"] {
            expect_status(&mut socket, legacy, status, None).await;
        }
    };
    let client = async {
        let client = station(&listener, &directory, "mode='legacy'\nmaximum_bytes=16").await;
        register(&client).await;
        client
    };
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(20),
        Box::pin(async { tokio::join!(peer, client) }),
    )
    .await
    .unwrap();
    assert_eq!(client.firmware16().unwrap().snapshot()["active"], false);
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}
