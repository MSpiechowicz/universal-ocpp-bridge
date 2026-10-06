//! A killed simulator process resumes its durable firmware job without duplicate terminal status.
mod firmware16_support;
#[allow(dead_code)]
#[path = "local_authorization16/socket.rs"]
mod socket;
use axum::{Router, routing::get};
use firmware16_support::{TestPki, private_directory, timestamp};
use serde_json::{Value, json};
use socket::{accept, boot, closed, receive, send};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::{net::TcpListener, time::timeout};

const IMAGE: &[u8] = b"durable process firmware image 122";
const SIGNED: &str = "SignedFirmwareStatusNotification";

struct Process(Option<Child>);

impl Drop for Process {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn private(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn launch(directory: &Path, scenario: &str) -> Process {
    Process(Some(
        Command::new(env!("CARGO_BIN_EXE_uob-sim"))
            .args(["run", "--config"])
            .arg(directory.join("config.toml"))
            .arg("--scenario")
            .arg(directory.join(scenario))
            .args(["--format", "jsonl"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ))
}

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

type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

async fn expect(socket: &mut Socket, status: &str) {
    let frame = receive(socket).await;
    assert_eq!(frame[2], SIGNED, "{frame}");
    assert_eq!(frame[3], json!({"status": status, "requestId": 7}));
    send(socket, json!([3, frame[1], {}])).await;
}

fn state(directory: &Path) -> Value {
    serde_json::from_slice(&fs::read(directory.join("firmware.json")).unwrap()).unwrap()
}

const CONNECT_BOOT: &str = "schema_version=1\nseed=122\n\n[[steps]]\nid='connect'\nstation='alpha'\naction='connect'\ntimeout_ms=5000\n\n[[steps]]\nid='boot'\nstation='alpha'\naction='boot'\ntimeout_ms=5000\nfixture_id='wire.ocpp16.boot.valid'\npayload={chargePointVendor='UOB',chargePointModel='Firmware 16'}\n";

#[tokio::test]
#[allow(clippy::too_many_lines)] // One live socket narrative; splitting would hide state carried across steps.
async fn killed_binary_resumes_signed_download_and_reports_installed_once() {
    let directory = private_directory("firmware-process");
    let pki = TestPki::generate();
    fs::write(directory.join("root.pem"), &pki.root_pem).unwrap();
    let base = artifacts().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    private(
        &directory.join("config.toml"),
        &format!(
            "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}'\nocpp_version='1.6'\nrequest_timeout_ms=5000\n[stations.firmware16]\nprivate_state_file='{}'\nmode='signed'\nmanufacturer_root_file='{}'\n",
            listener.local_addr().unwrap(),
            directory.join("firmware.json").display(),
            directory.join("root.pem").display()
        ),
    );
    private(
        &directory.join("first.toml"),
        &(CONNECT_BOOT.to_owned()
            + "\n[[steps]]\nid='downloading'\nstation='alpha'\naction='await_firmware'\ntimeout_ms=5000\nexpect_response={statuses=['Downloading'],pendingStatuses=0}\n\n[[steps]]\nid='park'\nstation='alpha'\naction='wait'\nduration_ms=60000\ntimeout_ms=65000\n"),
    );
    let mut first = launch(&directory, "first.toml");
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    let signature = pki.sign(IMAGE);
    let request = json!({"requestId": 7, "firmware": {
        "location": format!("{base}/image.bin"),
        "retrieveDateTime": timestamp(time::OffsetDateTime::now_utc()),
        "signingCertificate": pki.certificate_pem, "signature": signature}});
    send(
        &mut socket,
        json!([2, "signed", "SignedUpdateFirmware", request]),
    )
    .await;
    assert_eq!(
        receive(&mut socket).await,
        json!([3,"signed",{"status":"Accepted"}])
    );
    expect(&mut socket, "Downloading").await;
    timeout(Duration::from_secs(5), async {
        while state(&directory)["outbox"] != json!([]) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    first.0.as_mut().unwrap().kill().unwrap();
    let first_output = first.0.take().unwrap().wait_with_output().unwrap();
    drop(socket);
    assert_eq!(state(&directory)["job"]["phase"], "downloading");

    private(
        &directory.join("second.toml"),
        &format!(
            "{CONNECT_BOOT}\n[[steps]]\nid='installed'\nstation='alpha'\naction='await_firmware'\ntimeout_ms=20000\nexpect_response={{lastStatus='Installed',reboots=1,pendingStatuses=0,active=false}}\n\n[[steps]]\nid='disconnect'\nstation='alpha'\naction='disconnect'\ntimeout_ms=5000\n"
        ),
    );
    let mut second = launch(&directory, "second.toml");
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
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
    assert!(
        rebooted[3]["firmwareVersion"]
            .as_str()
            .unwrap()
            .starts_with("sha256-")
    );
    send(&mut socket, json!([3,rebooted[1],{"status":"Accepted","currentTime":"2026-10-06T00:00:00Z","interval":60}])).await;
    expect(&mut socket, "Installed").await;
    closed(&mut socket).await;
    timeout(Duration::from_secs(10), async {
        while second.0.as_mut().unwrap().try_wait().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let output = second.0.take().unwrap().wait_with_output().unwrap();
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
    fs::remove_dir_all(directory).unwrap();
}
