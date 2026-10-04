use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

#[path = "local_authorization16/socket.rs"]
mod socket;
use socket::{accept, boot, closed, receive, send};

const BOUND: Duration = Duration::from_secs(10);
const TRANSACTION_ID: i64 = 2_147_483_758;

struct Fixture {
    directory: PathBuf,
    child: Option<Child>,
}
impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("uob-ordinary-reconnect-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            directory,
            child: None,
        }
    }
    fn write(&self, name: &str, content: &str) -> PathBuf {
        let path = self.directory.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }
    fn start(&mut self, address: std::net::SocketAddr, scenario: &str) {
        let configuration = self.write("configuration.toml", &format!(
            "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{address}'\nocpp_version='1.6'\nreconnect=true\nrequest_timeout_ms=2000\n"));
        let scenario = self.write("scenario.toml", scenario);
        self.child = Some(
            Command::new(env!("CARGO_BIN_EXE_uob-sim"))
                .args(["run", "--config"])
                .arg(configuration)
                .arg("--scenario")
                .arg(scenario)
                .args(["--format", "jsonl"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    async fn finish(&mut self) -> Output {
        timeout(BOUND, async {
            while self.child.as_mut().unwrap().try_wait().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        self.child.take().unwrap().wait_with_output().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[derive(Clone, Copy)]
enum BootStatus {
    Accepted,
    Pending,
    Rejected,
}
impl BootStatus {
    fn native(self) -> &'static str {
        match self {
            Self::Accepted => "Accepted",
            Self::Pending => "Pending",
            Self::Rejected => "Rejected",
        }
    }
    fn accepted(self) -> bool {
        matches!(self, Self::Accepted)
    }
}

fn step(id: &str, action: &str, fields: &str) -> String {
    format!(
        "\n[[steps]]\nid='{id}'\nstation='alpha'\naction='{action}'\ntimeout_ms=5000\n{fields}\n"
    )
}
fn scenario(status: BootStatus) -> String {
    let mut text = "schema_version=1\nseed=111\n".to_owned();
    text += &step("connect", "connect", "");
    text += &step(
        "boot",
        "boot",
        &format!(
            "fixture_id='sim.ocpp16.autoreconnect.boot.{}'\npayload={{chargePointVendor='UOB',chargePointModel='Simulator'}}\nexpect_response={{status='{}',currentTime='2026-10-04T00:00:00Z',interval=60}}",
            status.native().to_lowercase(),
            status.native()
        ),
    );
    if status.accepted() {
        text += &step(
            "authorize",
            "authorize",
            "fixture_id='sim.ocpp16.authorize.accepted'\npayload={idTag='reconnect-private'}\nexpect_response={idTagInfo={status='Accepted'}}",
        );
        text += &step(
            "start",
            "start_transaction",
            &format!(
                "fixture_id='sim.ocpp16.transaction.start'\npayload={{connectorId=1,idTag='reconnect-private',meterStart=10,timestamp='2026-10-04T00:00:01Z'}}\nexpect_response={{transactionId={TRANSACTION_ID},idTagInfo={{status='Accepted'}}}}"
            ),
        );
        // The peer sends this only on the actual second socket. No timer or
        // explicit connect step can substitute for SDK automatic reconnection.
        text += &step(
            "second-socket-remote-stop",
            "await_remote_stop",
            &format!(
                "expect_response={{accepted=true,request={{transactionId={TRANSACTION_ID}}}}}"
            ),
        );
        text += &step(
            "stop",
            "stop_transaction",
            &format!(
                "fixture_id='sim.ocpp16.transaction.stop'\npayload={{transactionId={TRANSACTION_ID},meterStop=20,timestamp='2026-10-04T00:00:02Z',reason='Remote'}}\nexpect_response={{}}"
            ),
        );
    } else {
        text += &step(
            "second-socket-remote-start",
            "await_remote_start",
            "expect_response={accepted=true,request={connectorId=1,idTag='reconnect-private'}}",
        );
        text += &step(
            "must-stay-unregistered",
            "authorize",
            "fixture_id='sim.ocpp16.authorize.accepted'\npayload={idTag='reconnect-private'}",
        );
    }
    text
}

#[tokio::test]
async fn actual_cli_ordinary_autoreconnect_retains_accepted_boot_and_stops_original_transaction() {
    run(BootStatus::Accepted).await;
}
#[tokio::test]
async fn actual_cli_ordinary_autoreconnect_does_not_register_pending_boot() {
    run(BootStatus::Pending).await;
}
#[tokio::test]
async fn actual_cli_ordinary_autoreconnect_does_not_register_rejected_boot() {
    run(BootStatus::Rejected).await;
}

async fn run(status: BootStatus) {
    let mut fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = tokio::spawn(native_peer(listener, status));
    fixture.start(address, &scenario(status));
    let output = fixture.finish().await;
    for bytes in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains("reconnect-private"));
    }
    let events: Vec<Value> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let last = events.last().unwrap();
    // Diagnose the typed consumer result before awaiting peer assertions.
    assert_eq!(
        last["event"],
        if status.accepted() {
            "run_passed"
        } else {
            "run_failed"
        },
        "{last}"
    );
    let saw_stop = timeout(BOUND, peer).await.unwrap().unwrap();
    if status.accepted() {
        assert!(
            output.status.success(),
            "a genuinely Accepted ordinary registration must survive same-station automatic reconnection; final event: {last}"
        );
        assert_eq!(last["event"], "run_passed");
        assert!(
            saw_stop,
            "original confirmed transaction must stop on the second native socket"
        );
    } else {
        assert!(!output.status.success());
        assert_eq!(last["failure_code"], "station_not_registered");
        assert!(!saw_stop);
    }
    assert!(!fixture.directory.join("state.json").exists());
}

async fn native_peer(listener: TcpListener, status: BootStatus) -> bool {
    let mut socket = accept(&listener).await;
    if status.accepted() {
        boot(&mut socket).await;
    } else {
        let request = receive(&mut socket).await;
        assert_eq!(request[2], "BootNotification");
        send(
            &mut socket,
            json!([3,request[1],{"status":status.native(),
            "currentTime":"2026-10-04T00:00:00Z","interval":60}]),
        )
        .await;
    }
    if status.accepted() {
        let authorize = receive(&mut socket).await;
        assert_eq!(authorize[2], "Authorize");
        send(
            &mut socket,
            json!([3,authorize[1],{"idTagInfo":{"status":"Accepted"}}]),
        )
        .await;
        let start = receive(&mut socket).await;
        assert_eq!(start[2], "StartTransaction");
        assert_eq!(start[3]["idTag"], "reconnect-private");
        send(
            &mut socket,
            json!([3,start[1],{"transactionId":TRANSACTION_ID,
            "idTagInfo":{"status":"Accepted"}}]),
        )
        .await;
    }
    socket.close(None).await.unwrap();
    let mut socket = accept(&listener).await;
    let action = if status.accepted() {
        "RemoteStopTransaction"
    } else {
        "RemoteStartTransaction"
    };
    let payload = if status.accepted() {
        json!({"transactionId":TRANSACTION_ID})
    } else {
        json!({"connectorId":1,"idTag":"reconnect-private"})
    };
    send(
        &mut socket,
        json!([2, "actual-second-socket", action, payload]),
    )
    .await;
    let mut saw_stop = false;
    let mut saw_acknowledgement = false;
    timeout(BOUND, async {
        while let Some(message) = socket.next().await {
            match message.unwrap() {
                Message::Close(_) => break,
                Message::Text(text) => {
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    if frame[0] == 3 {
                        assert_eq!(frame[1], "actual-second-socket");
                        assert_eq!(frame[2]["status"], "Accepted");
                        saw_acknowledgement = true;
                    } else {
                        assert!(
                            status.accepted(),
                            "a Pending/Rejected Boot must not gain registration on reconnect"
                        );
                        assert_eq!(frame[0], 2);
                        assert_eq!(
                            frame[2], "StopTransaction",
                            "ordinary reconnect must not add an implicit Boot"
                        );
                        assert_eq!(frame[3]["transactionId"], TRANSACTION_ID);
                        assert_eq!(frame[3]["meterStop"], 20);
                        assert_eq!(frame[3]["timestamp"], "2026-10-04T00:00:02Z");
                        assert_eq!(frame[3]["reason"], "Remote");
                        assert!(!saw_stop);
                        saw_stop = true;
                        send(&mut socket, json!([3, frame[1], {}])).await;
                    }
                }
                _ => {}
            }
            if saw_acknowledgement && (!status.accepted() || saw_stop) {
                closed(&mut socket).await;
                break;
            }
        }
    })
    .await
    .unwrap();
    saw_stop
}
