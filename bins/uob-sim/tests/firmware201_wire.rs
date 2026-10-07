//! 2.0.1 firmware station behavior over a real OCPP-J socket and a local HTTP artifact server.
mod firmware201_support;
#[allow(dead_code)]
#[path = "local_authorization201/socket.rs"]
mod socket;
use axum::{Router, http::StatusCode, routing::get};
use firmware201_support::{TestPki, private_directory};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use socket::{accept, boot, closed, receive, send};
use std::{fmt::Write as _, path::Path, time::Duration};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use uob_sim::scenario::parse_configuration;
use uob_sim::{ProtocolClient, SimulatorAction, SimulatorCall, SimulatorProtocolClient};

type Socket = WebSocketStream<TcpStream>;
const IMAGE: &[u8] = b"independent 2.0.1 wire firmware image 123";
const STATUS: &str = "FirmwareStatusNotification";

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
    let firmware = if firmware.is_empty() {
        String::new()
    } else {
        format!(
            "[stations.firmware201]\nprivate_state_file='{}'\n{firmware}\n",
            directory.join("firmware.json").display()
        )
    };
    let source = format!(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}/ocpp/alpha'\nocpp_version='2.0.1'\nreconnect=true\n[[stations.evses]]\nid=1\nconnectors=[1]\n{firmware}",
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
            payload: json!({"reason":"PowerUp","chargingStation":{"model":"Firmware201","vendorName":"UOB","firmwareVersion":"sim-1.0.0"}}),
        })
        .await
        .unwrap();
    client
}

/// Acknowledge exactly the next station CALL, which must be this firmware status.
async fn expect_status(socket: &mut Socket, status: &str, request_id: Option<i32>) {
    let frame = receive(socket).await;
    assert_eq!(
        (frame[0].as_u64(), frame[2].as_str()),
        (Some(2), Some(STATUS)),
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
    assert_eq!(frame[1], id);
    frame
}

fn moment(offset: time::Duration) -> String {
    (OffsetDateTime::now_utc().replace_nanosecond(0).unwrap() + offset)
        .format(&Rfc3339)
        .unwrap()
}

fn request(id: i32, location: &str, retrieve: &str, signing: Option<(&TestPki, &[u8])>) -> Value {
    let mut firmware = json!({"location": location, "retrieveDateTime": retrieve});
    if let Some((pki, image)) = signing {
        firmware["signingCertificate"] = pki.certificate_pem.clone().into();
        firmware["signature"] = pki.sign(image).into();
    }
    json!({"requestId": id, "retries": 1, "retryInterval": 0, "firmware": firmware})
}

/// The rebooted station registers again, reporting the reason and the installed version.
async fn rebooted(listener: &TcpListener, socket: &mut Socket) -> Socket {
    closed(socket).await;
    let mut socket = accept(listener).await;
    let boot = receive(&mut socket).await;
    assert_eq!(boot[2], "BootNotification");
    assert_eq!(boot[3]["reason"], "FirmwareUpdate");
    let version =
        Sha256::digest(IMAGE)[..6]
            .iter()
            .fold("sha256-".to_owned(), |mut version, byte| {
                write!(version, "{byte:02x}").unwrap();
                version
            });
    assert_eq!(boot[3]["chargingStation"]["firmwareVersion"], version);
    send(
        &mut socket,
        json!([3,boot[1],{"status":"Accepted","currentTime":"2026-10-07T00:00:00Z","interval":60}]),
    )
    .await;
    socket
}

async fn trigger(socket: &mut Socket, id: &str) {
    let reply = call(
        socket,
        id,
        "TriggerMessage",
        json!({"requestedMessage": STATUS}),
    )
    .await;
    assert_eq!(reply[2]["status"], "Accepted");
}

#[tokio::test]
async fn secure_station_validates_installs_reboots_and_answers_triggers_and_cancellation() {
    let directory = private_directory("firmware201-wire-secure");
    let pki = TestPki::generate();
    let stranger = TestPki::generate();
    std::fs::write(directory.join("root.pem"), &pki.root_pem).unwrap();
    let base = artifacts(IMAGE).await;
    let location = format!("{base}/firmware.bin");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let now = moment(time::Duration::ZERO);
        let untrusted = request(40, &location, &now, Some((&stranger, IMAGE)));
        assert_eq!(
            call(&mut socket, "untrusted", "UpdateFirmware", untrusted).await[2]["status"],
            "InvalidCertificate"
        );
        let valid = request(41, &location, &now, Some((&pki, IMAGE)));
        assert_eq!(
            call(&mut socket, "valid", "UpdateFirmware", valid).await[2],
            json!({"status":"Accepted"})
        );
        for status in [
            "Downloading",
            "Downloaded",
            "SignatureVerified",
            "Installing",
            "InstallRebooting",
        ] {
            expect_status(&mut socket, status, Some(41)).await;
        }
        let mut socket = rebooted(&listener, &mut socket).await;
        expect_status(&mut socket, "Installed", Some(41)).await;
        // L01.FR.25: Idle after Installed, without a requestId.
        trigger(&mut socket, "idle").await;
        expect_status(&mut socket, "Idle", None).await;
        let later = moment(time::Duration::hours(1));
        let first = request(42, &location, &later, Some((&pki, IMAGE)));
        assert_eq!(
            call(&mut socket, "first", "UpdateFirmware", first).await[2]["status"],
            "Accepted"
        );
        expect_status(&mut socket, "DownloadScheduled", Some(42)).await;
        // L01.FR.26: otherwise the last sent status with its requestId.
        trigger(&mut socket, "last").await;
        expect_status(&mut socket, "DownloadScheduled", Some(42)).await;
        let second = request(43, &location, &later, Some((&pki, IMAGE)));
        assert_eq!(
            call(&mut socket, "second", "UpdateFirmware", second).await[2]["status"],
            "AcceptedCanceled"
        );
        expect_status(&mut socket, "DownloadScheduled", Some(43)).await;
        let mut strict = request(44, &location, &later, Some((&pki, IMAGE)));
        strict["unexpected"] = true.into();
        assert_eq!(
            call(&mut socket, "strict", "UpdateFirmware", strict).await[2],
            "FormationViolation"
        );
    };
    let config = format!(
        "mode='secure'\nmanufacturer_root_file='{}'",
        directory.join("root.pem").display()
    );
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(30),
        Box::pin(async { tokio::join!(peer, station(&listener, &directory, &config)) }),
    )
    .await
    .unwrap();
    let snapshot = client.firmware201().unwrap().snapshot();
    assert_eq!(snapshot["requestId"], 43);
    assert_eq!(snapshot["cancelled"], json!([42]));
    assert_eq!(snapshot["reboots"], 1);
    let traces = format!("{:?}", client.traces());
    assert!(!traces.contains(&base) && !traces.contains("BEGIN CERTIFICATE"));
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn non_secure_station_retries_failed_downloads_and_unconfigured_station_declines() {
    let directory = private_directory("firmware201-wire-plain");
    let base = artifacts(IMAGE).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let now = moment(time::Duration::ZERO);
        let missing = request(-7, &format!("{base}/missing.bin"), &now, None);
        assert_eq!(
            call(&mut socket, "missing", "UpdateFirmware", missing).await[2]["status"],
            "Accepted"
        );
        // L01.FR.30: one Downloading per attempt, then DownloadFailed.
        for status in ["Downloading", "Downloading", "DownloadFailed"] {
            expect_status(&mut socket, status, Some(-7)).await;
        }
        let plain = request(8, &format!("{base}/firmware.bin"), &now, None);
        assert_eq!(
            call(&mut socket, "plain", "UpdateFirmware", plain).await[2]["status"],
            "Accepted"
        );
        for status in [
            "Downloading",
            "Downloaded",
            "Installing",
            "InstallRebooting",
        ] {
            expect_status(&mut socket, status, Some(8)).await;
        }
        let mut socket = rebooted(&listener, &mut socket).await;
        expect_status(&mut socket, "Installed", Some(8)).await;
    };
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(30),
        Box::pin(async { tokio::join!(peer, station(&listener, &directory, "mode='non_secure'")) }),
    )
    .await
    .unwrap();
    assert_eq!(client.firmware201().unwrap().snapshot()["active"], false);
    client.abort();

    // Without a firmware model, the client library keeps its NotImplemented reply.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let any = request(
            9,
            &format!("{base}/firmware.bin"),
            &moment(time::Duration::ZERO),
            None,
        );
        let reply = call(&mut socket, "unconfigured", "UpdateFirmware", any).await;
        assert_eq!(
            (reply[0].clone(), reply[2].clone()),
            (json!(4), json!("NotImplemented"))
        );
    };
    let ((), client) = tokio::time::timeout(
        Duration::from_secs(10),
        Box::pin(async { tokio::join!(peer, station(&listener, &directory, "")) }),
    )
    .await
    .unwrap();
    assert!(client.firmware201().is_none());
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}
