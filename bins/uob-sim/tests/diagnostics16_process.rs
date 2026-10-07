//! A killed simulator process resumes its durable log upload without a duplicate end status.
mod diagnostics16_support;
#[allow(dead_code)]
#[path = "local_authorization16/socket.rs"]
mod socket;
use axum::{Router, body::Bytes, extract::Path as UrlPath, routing::put};
use diagnostics16_support::private_directory;
use serde_json::{Value, json};
use socket::{accept, boot, closed, receive, send};
use std::sync::{
    Arc, Mutex,
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

const LOG: &str = "LogStatusNotification";

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

type Stored = Arc<Mutex<Vec<(String, usize)>>>;

/// The first upload hangs so the process dies mid-transfer; later uploads are stored.
async fn receiver(stored: Stored) -> String {
    let requests = Arc::new(AtomicUsize::new(0));
    let router = Router::new().route(
        "/uploads/{slot}/{name}",
        put(
            move |UrlPath((slot, name)): UrlPath<(String, String)>, body: Bytes| {
                let requests = Arc::clone(&requests);
                let stored = Arc::clone(&stored);
                async move {
                    if requests.fetch_add(1, Ordering::SeqCst) == 0 {
                        tokio::time::sleep(Duration::from_secs(120)).await;
                    }
                    stored
                        .lock()
                        .unwrap()
                        .push((format!("{slot}/{name}"), body.len()));
                    axum::http::StatusCode::CREATED
                }
            },
        ),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    base
}

type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

async fn expect(socket: &mut Socket, status: &str) {
    let frame = receive(socket).await;
    assert_eq!(frame[2], LOG, "{frame}");
    assert_eq!(frame[3], json!({"status": status, "requestId": 31}));
    send(socket, json!([3, frame[1], {}])).await;
}

fn state(directory: &Path) -> Value {
    serde_json::from_slice(&fs::read(directory.join("diagnostics.json")).unwrap()).unwrap()
}

const CONNECT_BOOT: &str = "schema_version=1\nseed=124\n\n[[steps]]\nid='connect'\nstation='alpha'\naction='connect'\ntimeout_ms=5000\n\n[[steps]]\nid='boot'\nstation='alpha'\naction='boot'\ntimeout_ms=5000\nfixture_id='wire.ocpp16.boot.valid'\npayload={chargePointVendor='UOB',chargePointModel='Diagnostics 16'}\n";

#[tokio::test]
async fn killed_binary_resumes_security_log_upload_and_reports_uploaded_once() {
    let directory = private_directory("diagnostics-process");
    let stored = Stored::default();
    let base = receiver(Arc::clone(&stored)).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    private(
        &directory.join("config.toml"),
        &format!(
            "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}'\nocpp_version='1.6'\nrequest_timeout_ms=5000\n[stations.diagnostics16]\nprivate_state_file='{}'\nlegacy=false\nsecurity_log=true\n",
            listener.local_addr().unwrap(),
            directory.join("diagnostics.json").display(),
        ),
    );
    private(
        &directory.join("first.toml"),
        &(CONNECT_BOOT.to_owned()
            + "\n[[steps]]\nid='uploading'\nstation='alpha'\naction='await_diagnostics'\ntimeout_ms=5000\nexpect_response={statuses=['Uploading'],pendingStatuses=0}\n\n[[steps]]\nid='park'\nstation='alpha'\naction='wait'\nduration_ms=60000\ntimeout_ms=65000\n"),
    );
    let mut first = launch(&directory, "first.toml");
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    let location = format!("{base}/uploads/security/");
    let request = json!({"logType": "SecurityLog", "requestId": 31,
        "log": {"remoteLocation": location}});
    send(&mut socket, json!([2, "log", "GetLog", request])).await;
    assert_eq!(
        receive(&mut socket).await,
        json!([3,"log",{"status":"Accepted","filename":"securitylog-alpha-1.log"}])
    );
    expect(&mut socket, "Uploading").await;
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
    assert_eq!(state(&directory)["job"]["phase"], "uploading");
    assert!(stored.lock().unwrap().is_empty());

    private(
        &directory.join("second.toml"),
        &format!(
            "{CONNECT_BOOT}\n[[steps]]\nid='uploaded'\nstation='alpha'\naction='await_diagnostics'\ntimeout_ms=20000\nexpect_response={{lastStatus='Uploaded',uploads=1,pendingStatuses=0,active=false,requestId=31}}\n\n[[steps]]\nid='disconnect'\nstation='alpha'\naction='disconnect'\ntimeout_ms=5000\n"
        ),
    );
    let mut second = launch(&directory, "second.toml");
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    // The restarted attempt reports Uploading again (N01.FR.10 allows one per attempt).
    expect(&mut socket, "Uploading").await;
    expect(&mut socket, "Uploaded").await;
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
        assert!(
            !String::from_utf8_lossy(bytes).contains(&base),
            "process output leaked the upload location"
        );
    }
    assert_eq!(
        *stored.lock().unwrap(),
        [("security/securitylog-alpha-1.log".to_owned(), 2048)]
    );
    let after = state(&directory);
    assert_eq!(after["job"]["phase"], "finished");
    assert_eq!(after["job"]["attempts"], 1);
    assert_eq!(after["uploads"], 1);
    fs::remove_dir_all(directory).unwrap();
}
