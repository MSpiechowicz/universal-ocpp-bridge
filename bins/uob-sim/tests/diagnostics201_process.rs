//! A killed 2.0.1 simulator process resumes its durable log upload without a duplicate end status.
#[allow(dead_code)] // Shared helpers also serve the local-authorization process targets.
#[path = "local_authorization201/process.rs"]
mod common;
use axum::{Router, body::Bytes, extract::Path as UrlPath, routing::put};
use common::{
    PrivateDirectory, Process, accept, boot, closed, native_boot, prefix, receive, send, step,
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::{fs, time::Duration};
use tokio::{net::TcpListener, time::timeout};

const LOG: &str = "LogStatusNotification";
type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;
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

async fn expect(socket: &mut Socket, status: &str) {
    let frame = receive(socket).await;
    assert_eq!(frame[2], LOG, "{frame}");
    assert_eq!(frame[3], json!({"status": status, "requestId": 31}));
    send(socket, json!([3, frame[1], {}])).await;
}

fn state(directory: &PrivateDirectory) -> Value {
    serde_json::from_slice(&fs::read(directory.0.join("diagnostics.json")).unwrap()).unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One live socket narrative; splitting would hide state carried across steps.
async fn killed_binary_resumes_security_log_upload_and_reports_uploaded_once() {
    let directory = PrivateDirectory::new();
    let stored = Stored::default();
    let base = receiver(Arc::clone(&stored)).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = directory.write(
        "config.toml",
        &format!(
            "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}'\nocpp_version='2.0.1'\nrequest_timeout_ms=5000\nreconnect=true\n[[stations.evses]]\nid=1\nconnectors=[1]\n[stations.diagnostics201]\nprivate_state_file='{}'\n",
            listener.local_addr().unwrap(),
            directory.0.join("diagnostics.json").display(),
        ),
    );
    let first = directory.write(
        "first.toml",
        &(prefix()
            + &step("connect", "connect", "")
            + &native_boot("boot", "Accepted")
            + &step("uploading", "await_diagnostics", "expect_response={statuses=['Uploading'],pendingStatuses=0}")
            + "\n[[steps]]\nid='park'\nstation='alpha'\naction='wait'\nduration_ms=60000\ntimeout_ms=65000\n"),
    );
    let mut first = Process::start(&config, &first);
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    let request = json!({"logType": "SecurityLog", "requestId": 31,
        "log": {"remoteLocation": format!("{base}/uploads/security/")}});
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
    let first_output = first.kill();
    drop(socket);
    assert_eq!(state(&directory)["job"]["phase"], "uploading");
    assert!(stored.lock().unwrap().is_empty());

    let second = directory.write(
        "second.toml",
        &(prefix()
            + &step("connect", "connect", "")
            + &native_boot("boot", "Accepted")
            + &step("uploaded", "await_diagnostics", "expect_response={lastStatus='Uploaded',uploads=1,pendingStatuses=0,active=false,requestId=31}")
            + &step("disconnect", "disconnect", "")),
    );
    let mut second = Process::start(&config, &second);
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    // The restarted attempt reports Uploading again (N01.FR.10 allows one per attempt).
    expect(&mut socket, "Uploading").await;
    expect(&mut socket, "Uploaded").await;
    closed(&mut socket).await;
    let output = second.finish().await;
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
}
