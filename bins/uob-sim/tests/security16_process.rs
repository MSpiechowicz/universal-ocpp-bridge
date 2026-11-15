//! A killed simulator process resends its unacknowledged security event from the durable queue.
mod security16_support;
#[allow(dead_code)]
#[path = "local_authorization16/socket.rs"]
mod socket;
use security16_support::private_directory;
use serde_json::{Value, json};
use socket::{accept, boot, closed, receive, send};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::{net::TcpListener, time::timeout};

const TECH_INFO: &str = "cover switch opened";

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

fn state(directory: &Path) -> Value {
    serde_json::from_slice(&fs::read(directory.join("security.json")).unwrap()).unwrap()
}

const CONNECT_BOOT: &str = "schema_version=1\nseed=127\n\n[[steps]]\nid='connect'\nstation='alpha'\naction='connect'\ntimeout_ms=5000\n\n[[steps]]\nid='boot'\nstation='alpha'\naction='boot'\ntimeout_ms=5000\nfixture_id='wire.ocpp16.boot.valid'\npayload={chargePointVendor='UOB',chargePointModel='Security 16'}\n";

#[tokio::test]
async fn killed_binary_resends_the_unacknowledged_event_from_its_durable_queue() {
    let directory = private_directory("security-process");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    private(
        &directory.join("config.toml"),
        &format!(
            "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}'\nocpp_version='1.6'\nrequest_timeout_ms=5000\n[stations.security16]\nprivate_state_file='{}'\n",
            listener.local_addr().unwrap(),
            directory.join("security.json").display(),
        ),
    );
    private(
        &directory.join("first.toml"),
        &format!(
            "{CONNECT_BOOT}\n[[steps]]\nid='emit'\nstation='alpha'\naction='emit_security_event'\ntimeout_ms=5000\npayload={{type='TamperDetectionActivated',timestamp='2026-10-01T12:00:00Z',techInfo='{TECH_INFO}'}}\n\n[[steps]]\nid='park'\nstation='alpha'\naction='wait'\nduration_ms=60000\ntimeout_ms=65000\n"
        ),
    );
    let mut first = launch(&directory, "first.toml");
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    let lost = receive(&mut socket).await;
    assert_eq!(lost[2], "SecurityEventNotification", "{lost}");
    let expected = json!({"type": "TamperDetectionActivated", "timestamp": "2026-10-01T12:00:00Z", "techInfo": TECH_INFO});
    assert_eq!(lost[3], expected);
    // The acknowledgement is withheld and the process dies with the event still queued.
    first.0.as_mut().unwrap().kill().unwrap();
    let first_output = first.0.take().unwrap().wait_with_output().unwrap();
    drop(socket);
    let persisted = state(&directory);
    assert_eq!(persisted["outbox"].as_array().unwrap().len(), 1);
    assert_eq!(persisted["outbox"][0]["type"], "TamperDetectionActivated");
    assert_eq!(persisted["acknowledged"], 0);

    private(
        &directory.join("second.toml"),
        &format!(
            "{CONNECT_BOOT}\n[[steps]]\nid='acknowledged'\nstation='alpha'\naction='await_security'\ntimeout_ms=20000\nexpect_response={{pendingEvents=0,eventsEmitted=1,eventsAcknowledged=1,lastAcknowledgedType='TamperDetectionActivated'}}\n\n[[steps]]\nid='disconnect'\nstation='alpha'\naction='disconnect'\ntimeout_ms=5000\n"
        ),
    );
    let mut second = launch(&directory, "second.toml");
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    // The restarted process registers first, then resends the very same event.
    let resent = receive(&mut socket).await;
    assert_eq!(resent[2], "SecurityEventNotification", "{resent}");
    assert_eq!(resent[3], expected);
    send(&mut socket, json!([3, resent[1], {}])).await;
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
            !String::from_utf8_lossy(bytes).contains(TECH_INFO),
            "process output leaked the technical detail"
        );
    }
    let after = state(&directory);
    assert_eq!(after["outbox"], json!([]));
    assert_eq!(
        (&after["emitted"], &after["acknowledged"]),
        (&json!(1), &json!(1))
    );
    fs::remove_dir_all(directory).unwrap();
}
