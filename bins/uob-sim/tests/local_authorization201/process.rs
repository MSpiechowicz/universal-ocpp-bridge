use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::timeout;

#[path = "socket.rs"]
mod socket;
pub(crate) use socket::{accept, boot, closed, receive, send};

pub(crate) const BOUND: Duration = Duration::from_secs(10);

pub(crate) struct PrivateDirectory(pub(crate) PathBuf);
impl PrivateDirectory {
    pub(crate) fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("uob-native-process-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    pub(crate) fn write(&self, name: &str, content: &str) -> PathBuf {
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

pub(crate) struct Process(Option<Child>);
impl Process {
    pub(crate) fn start(config: &Path, scenario: &Path) -> Self {
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
    pub(crate) fn kill(&mut self) -> Output {
        self.0.as_mut().unwrap().kill().unwrap();
        self.0.take().unwrap().wait_with_output().unwrap()
    }
    pub(crate) async fn finish(&mut self) -> Output {
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

pub(crate) fn step(id: &str, action: &str, fields: &str) -> String {
    format!(
        "\n[[steps]]\nid='{id}'\nstation='alpha'\naction='{action}'\ntimeout_ms=5000\n{fields}\n"
    )
}
pub(crate) fn prefix() -> String {
    "schema_version=1\nseed=112\n".to_owned()
}
pub(crate) fn native_boot(id: &str, expected: &str) -> String {
    let interval = if expected == "Rejected" { 1 } else { 60 };
    step(
        id,
        "boot",
        &format!(
            "fixture_id='sim.ocpp201.boot.accepted'\npayload={{reason='PowerUp',chargingStation={{vendorName='UOB',model='Simulator'}}}}\nexpect_response={{status='{expected}',currentTime='2026-10-04T00:00:00Z',interval={interval}}}"
        ),
    )
}
pub(crate) fn start_fact() -> Value {
    json!({"eventType":"Started","timestamp":"2026-10-04T00:00:01.123+00:00","triggerReason":"Authorized","seqNo":0,"offline":true,
        "transactionInfo":{"transactionId":"original-native-112","chargingState":"Charging"},"evse":{"id":1,"connectorId":1},
        "idToken":{"idToken":"list-marker-112","type":"Central"}})
}
pub(crate) fn end_fact() -> Value {
    json!({"eventType":"Ended","timestamp":"2026-10-04T00:00:02.123+00:00","triggerReason":"StopAuthorized","seqNo":1,"offline":true,
        "transactionInfo":{"transactionId":"original-native-112","chargingState":"Idle","stoppedReason":"Local"},"evse":{"id":1,"connectorId":1}})
}
pub(crate) fn facts() -> String {
    step(
        "start",
        "offline_start",
        "payload={eventType='Started',timestamp='2026-10-04T00:00:01.123+00:00',triggerReason='Authorized',seqNo=0,offline=true,transactionInfo={transactionId='original-native-112',chargingState='Charging'},evse={id=1,connectorId=1},idToken={idToken='list-marker-112',type='Central'}}\nexpect_response={accepted=true}",
    ) + &step(
        "end",
        "offline_stop",
        "payload={eventType='Ended',timestamp='2026-10-04T00:00:02.123+00:00',triggerReason='StopAuthorized',seqNo=1,offline=true,transactionInfo={transactionId='original-native-112',chargingState='Idle',stoppedReason='Local'},evse={id=1,connectorId=1}}\nexpect_response={stopped=true}",
    )
}
pub(crate) fn assert_private_output(output: &Output) {
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes).to_lowercase();
        for secret in [
            "list-marker-112",
            "group-marker-112",
            "message-marker-112",
            "additional-marker-112",
            "custom-marker-112",
            "info-marker-112",
        ] {
            assert!(!text.contains(secret), "private marker leaked");
        }
    }
}
#[allow(dead_code)] // This shared helper is needed only by the recovery target.
pub(crate) async fn await_queue(state: &Path, count: usize) {
    timeout(BOUND, async {
        loop {
            if fs::read(state)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .is_some_and(|state| {
                    state["offline"]
                        .as_array()
                        .is_some_and(|records| records.len() == count)
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
