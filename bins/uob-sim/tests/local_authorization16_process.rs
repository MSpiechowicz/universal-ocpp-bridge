use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::timeout;

#[path = "local_authorization16/socket.rs"]
mod socket;
use socket::{accept, boot, closed, receive, send};

const BOUND: Duration = Duration::from_secs(10);

struct PrivateDirectory(PathBuf);
impl PrivateDirectory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("uob-native-process-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    fn write(&self, name: &str, content: &str) -> PathBuf {
        let path = self.0.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }
}
impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Process(Option<Child>);
impl Process {
    fn start(config: &Path, scenario: &Path) -> Self {
        Self(Some(
            Command::new(env!("CARGO_BIN_EXE_uob-sim"))
                .arg("run")
                .arg("--config")
                .arg(config)
                .arg("--scenario")
                .arg(scenario)
                .args(["--format", "jsonl"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ))
    }
    fn kill(&mut self) -> Output {
        self.0.as_mut().unwrap().kill().unwrap();
        self.0.take().unwrap().wait_with_output().unwrap()
    }
    async fn finish(&mut self) -> Output {
        timeout(BOUND, async {
            while self.0.as_mut().unwrap().try_wait().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn step(id: &str, action: &str, fields: &str) -> String {
    format!(
        "\n[[steps]]\nid='{id}'\nstation='alpha'\naction='{action}'\ntimeout_ms=5000\n{fields}\n"
    )
}
fn prefix() -> String {
    "schema_version=1\nseed=111\n".to_owned()
}
fn native_boot() -> String {
    step(
        "boot",
        "boot",
        "fixture_id='sim.ocpp16.boot.accepted'\npayload={chargePointVendor='UOB',chargePointModel='Simulator'}",
    )
}
fn offline_start() -> String {
    step(
        "offline-start",
        "offline_start",
        "payload={connectorId=1,idTag='list-marker-111',meterStart=1,timestamp='2026-10-04T00:00:01Z'}\nexpect_response={accepted=true}",
    )
}
fn offline_stop() -> String {
    step(
        "offline-stop",
        "offline_stop",
        "payload={connectorId=1,meterStop=2,timestamp='2026-10-04T00:00:02Z'}\nexpect_response={stopped=true}",
    )
}
fn assert_private_output(output: &Output) {
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes).to_lowercase();
        for secret in ["list-marker-111", "parent-marker-111"] {
            assert!(!text.contains(secret));
        }
    }
}

#[tokio::test]
async fn binary_kill_new_process_recovers_native_list_and_replays_private_queue() {
    let directory = PrivateDirectory::new();
    let state_path = directory.0.join("state.json");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = directory.write("configuration.toml", &format!(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{address}'\nocpp_version='1.6'\nrequest_timeout_ms=2000\n[stations.local_authorization]\nprivate_state_file='{}'\n", state_path.display()));
    let mut first = prefix();
    first += &step("connect", "connect", "");
    first += &native_boot();
    first += &step(
        "await-full",
        "await_local_authorization",
        "expect_response={listVersion=1,listEntries=1}",
    );
    first += &step("offline", "csms_offline", "");
    first += &offline_start();
    first += &offline_stop();
    first += "\n[[steps]]\nid='keep-process'\nstation='alpha'\naction='wait'\ntimeout_ms=61000\nduration_ms=60000\n";
    let first = directory.write("first.toml", &first);
    let mut second = prefix();
    second += &step("connect", "connect", "");
    second += &native_boot();
    second += &step(
        "assert-restored",
        "assert_local_authorization",
        "expect_response={listVersion=1,listEntries=1,cacheEntries=0,offlineRecords=0,uncertainRecords=0}",
    );
    second += &step("offline", "csms_offline", "");
    second += &offline_start();
    second += &offline_stop();
    second += &step(
        "assert-local-after-restart",
        "assert_local_authorization",
        "expect_response={offlineRecords=1,uncertainRecords=0}",
    );
    let second = directory.write("second.toml", &second);
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let full: Value = serde_json::from_str(include_str!(
            "../../../tests/ocpp-fixtures/corpus/wire/1.6/local-list-full.json"
        ))
        .unwrap();
        send(&mut socket, full).await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        closed(&mut socket).await;
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let start = receive(&mut socket).await;
        assert_eq!(start[2], "StartTransaction");
        assert_eq!(start[3]["idTag"], "list-marker-111");
        send(&mut socket, json!([3,start[1],{"transactionId":84,"idTagInfo":{"status":"Accepted","parentIdTag":"PARENT-MARKER-111"}}])).await;
        let stop = receive(&mut socket).await;
        assert_eq!(stop[2], "StopTransaction");
        assert_eq!(stop[3]["transactionId"], 84);
        send(&mut socket, json!([3, stop[1], {}])).await;
        closed(&mut socket).await;
    });
    let mut process = Process::start(&config, &first);
    timeout(BOUND, async {
        loop {
            let queued = fs::read(&state_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .is_some_and(|state| {
                    state["offline"]
                        .as_array()
                        .is_some_and(|records| records.len() == 1 && records[0]["stop"].is_object())
                });
            if queued {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_private_output(&process.kill());
    let mut process = Process::start(&config, &second);
    let output = process.finish().await;
    assert_private_output(&output);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::metadata(&state_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    timeout(BOUND, peer).await.unwrap().unwrap();
}

#[tokio::test]
async fn actual_cli_retires_a_fault_closed_client_and_reconnects_without_retrying_update() {
    let directory = PrivateDirectory::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = directory.0.join("state.json");
    let config = directory.write("fault.toml", &format!(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{address}'\nocpp_version='1.6'\nrequest_timeout_ms=2000\nreconnect=false\n[stations.local_authorization]\nprivate_state_file='{}'\n",
        state.display()
    ));
    let script = prefix()
        + &step("connect", "connect", "")
        + &native_boot()
        + &step("lose-native-ack", "drop_local_reply", "")
        + &step("armed-on-wire", "heartbeat", "")
        + &step(
            "actual-applied",
            "await_local_authorization",
            "expect_response={listVersion=22,listEntries=1,offlineRecords=0}",
        )
        + &step(
            "fresh-socket",
            "csms_reconnect",
            "expect_detail='registered_and_replayed'",
        )
        + &step(
            "fresh-full",
            "await_local_authorization",
            "expect_response={listVersion=23,listEntries=1,offlineRecords=0}",
        )
        + &step("disconnect", "disconnect", "");
    let scenario = directory.write("fault-scenario.toml", &script);
    let mut process = Process::start(&config, &scenario);
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    let heartbeat = receive(&mut socket).await;
    assert_eq!(heartbeat[2], "Heartbeat");
    send(
        &mut socket,
        json!([3,heartbeat[1],{"currentTime":"2026-10-04T00:00:00Z"}]),
    )
    .await;
    let update = |version| {
        json!({"listVersion":version,"updateType":"Full","localAuthorizationList":[{
            "idTag":"private-fault-marker","idTagInfo":{"status":"Accepted"}
        }]})
    };
    send(
        &mut socket,
        json!([2, "lost-update", "SendLocalList", update(22)]),
    )
    .await;
    closed(&mut socket).await;
    let persisted: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    assert_eq!(persisted["version"], 22);
    socket = accept(&listener).await;
    boot(&mut socket).await;
    send(
        &mut socket,
        json!([2, "after-loss-version", "GetLocalListVersion", {}]),
    )
    .await;
    assert_eq!(
        receive(&mut socket).await,
        json!([3,"after-loss-version",{"listVersion":22}])
    );
    send(
        &mut socket,
        json!([2, "fresh-resync", "SendLocalList", update(23)]),
    )
    .await;
    assert_eq!(
        receive(&mut socket).await,
        json!([3,"fresh-resync",{"status":"Accepted"}])
    );
    closed(&mut socket).await;
    let output = process.finish().await;
    assert!(
        output.status.success(),
        "actual fault/reconnect CLI scenario failed"
    );
    let public = String::from_utf8(output.stdout).unwrap();
    assert!(!public.contains("private-fault-marker"));
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("private-fault-marker")
    );
    assert!(public.lines().last().unwrap().contains("run_passed"));
}
