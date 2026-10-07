//! A killed 2.0.1 simulator process resumes its durable firmware job and reports Installed once.
#[allow(dead_code)] // Shared helpers also serve the local-authorization process targets.
#[path = "local_authorization201/process.rs"]
mod common;
mod firmware201_support;
use axum::{Router, routing::get};
use common::{
    PrivateDirectory, Process, accept, boot, closed, native_boot, prefix, receive, send, step,
};
use firmware201_support::TestPki;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::{fs, time::Duration};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{net::TcpListener, time::timeout};

const IMAGE: &[u8] = b"durable 2.0.1 process firmware image 123";
type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

/// The first download hangs so the process dies mid-transfer; later downloads succeed.
async fn artifacts() -> String {
    let requests = Arc::new(AtomicUsize::new(0));
    let router = Router::new().route(
        "/image.bin",
        get(move || {
            let requests = Arc::clone(&requests);
            async move {
                if requests.fetch_add(1, Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_secs(120)).await;
                }
                IMAGE
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    base
}

async fn expect(socket: &mut Socket, status: &str) {
    let frame = receive(socket).await;
    assert_eq!(frame[2], "FirmwareStatusNotification", "{frame}");
    assert_eq!(frame[3], json!({"status": status, "requestId": 7}));
    send(socket, json!([3, frame[1], {}])).await;
}

fn state(directory: &PrivateDirectory) -> Value {
    serde_json::from_slice(&fs::read(directory.0.join("firmware.json")).unwrap()).unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One live socket narrative; splitting would hide state carried across steps.
async fn killed_binary_resumes_secure_download_and_reports_installed_once() {
    let directory = PrivateDirectory::new();
    let pki = TestPki::generate();
    fs::write(directory.0.join("root.pem"), &pki.root_pem).unwrap();
    let base = artifacts().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = directory.write(
        "config.toml",
        &format!(
            "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}'\nocpp_version='2.0.1'\nrequest_timeout_ms=5000\nreconnect=true\n[[stations.evses]]\nid=1\nconnectors=[1]\n[stations.firmware201]\nprivate_state_file='{}'\nmode='secure'\nmanufacturer_root_file='{}'\n",
            listener.local_addr().unwrap(),
            directory.0.join("firmware.json").display(),
            directory.0.join("root.pem").display()
        ),
    );
    let first = directory.write(
        "first.toml",
        &(prefix()
            + &step("connect", "connect", "")
            + &native_boot("boot", "Accepted")
            + &step("downloading", "await_firmware", "expect_response={statuses=['Downloading'],pendingStatuses=0}")
            + "\n[[steps]]\nid='park'\nstation='alpha'\naction='wait'\nduration_ms=60000\ntimeout_ms=65000\n"),
    );
    let mut first = Process::start(&config, &first);
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    let signature = pki.sign(IMAGE);
    let retrieve = OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .unwrap()
        .format(&Rfc3339)
        .unwrap();
    let request = json!({"requestId": 7, "firmware": {
        "location": format!("{base}/image.bin"), "retrieveDateTime": retrieve,
        "signingCertificate": pki.certificate_pem, "signature": signature}});
    send(&mut socket, json!([2, "update", "UpdateFirmware", request])).await;
    assert_eq!(
        receive(&mut socket).await,
        json!([3,"update",{"status":"Accepted"}])
    );
    expect(&mut socket, "Downloading").await;
    timeout(Duration::from_secs(5), async {
        while state(&directory)["outbox"] != json!([]) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let first_output = first.kill();
    drop(socket);
    assert_eq!(state(&directory)["job"]["phase"], "downloading");

    let second = directory.write(
        "second.toml",
        &(prefix()
            + &step("connect", "connect", "")
            + &native_boot("boot", "Accepted")
            + &step("installed", "await_firmware", "expect_response={lastStatus='Installed',reboots=1,pendingStatuses=0,active=false,requestId=7}")
            + &step("disconnect", "disconnect", "")),
    );
    let mut second = Process::start(&config, &second);
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    // The interrupted attempt starts again; nothing already delivered is repeated.
    for status in [
        "Downloading",
        "Downloaded",
        "SignatureVerified",
        "Installing",
        "InstallRebooting",
    ] {
        expect(&mut socket, status).await;
    }
    closed(&mut socket).await;
    let mut socket = accept(&listener).await;
    let rebooted = receive(&mut socket).await;
    assert_eq!(rebooted[2], "BootNotification");
    assert_eq!(rebooted[3]["reason"], "FirmwareUpdate");
    assert!(
        rebooted[3]["chargingStation"]["firmwareVersion"]
            .as_str()
            .unwrap()
            .starts_with("sha256-")
    );
    send(&mut socket, json!([3,rebooted[1],{"status":"Accepted","currentTime":"2026-10-07T00:00:00Z","interval":60}])).await;
    expect(&mut socket, "Installed").await;
    closed(&mut socket).await;
    let output = second.finish().await;
    assert!(output.status.success(), "native process scenario failed");
    for bytes in [
        &first_output.stdout,
        &first_output.stderr,
        &output.stdout,
        &output.stderr,
    ] {
        let public = String::from_utf8_lossy(bytes);
        for marker in [base.as_str(), "BEGIN CERTIFICATE", &signature[..32]] {
            assert!(
                !public.contains(marker),
                "process output leaked private request material"
            );
        }
    }
    let after = state(&directory);
    assert_eq!(after["job"]["phase"], "finished");
    assert_eq!(after["reboots"], 1);
    assert_eq!(after["last_sent"], json!(["Installed", 7]));
}
