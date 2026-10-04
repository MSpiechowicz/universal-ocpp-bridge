use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use ocpp_client::ocpp_types::v16::common::SendLocalListResponseStatus;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::timeout;
use uob_sim::local_authorization::{LocalAuthorizationConfig, LocalAuthorizationHandle};

#[path = "local_authorization16/cache_recovery.rs"]
mod cache_recovery;
#[path = "local_authorization16/socket.rs"]
mod socket;
use socket::{accept, boot, closed, receive, send};

const BOUND: Duration = Duration::from_secs(10);
const TRANSACTION_ID: i64 = 2_147_483_758;

struct PrivateDirectory(PathBuf);
impl PrivateDirectory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("uob-native-independence-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    fn settings(&self) -> LocalAuthorizationConfig {
        LocalAuthorizationConfig {
            private_state_file: self.0.join("state.json").to_str().unwrap().to_owned(),
            list_supported: true,
            cache_supported: true,
        }
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
    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.0.join("state.json")).unwrap()).unwrap()
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
                .args(["run", "--config"])
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

fn retained_blocked_list(directory: &PrivateDirectory) {
    let model = LocalAuthorizationHandle::open("alpha", &directory.settings()).unwrap();
    let update = serde_json::from_value(json!({"listVersion":7,"updateType":"Full",
        "localAuthorizationList":[{"idTag":"cache-private","idTagInfo":{"status":"Blocked"}}]}))
    .unwrap();
    assert_eq!(model.update(&update), SendLocalListResponseStatus::Accepted);
    assert!(!model.authorize_offline("cache-private", time::OffsetDateTime::now_utc()));
}

#[test]
fn disabled_retained_list_does_not_suppress_independent_durable_cache() {
    let directory = PrivateDirectory::new();
    retained_blocked_list(&directory);
    let mut settings = directory.settings();
    settings.list_supported = false;
    let model = LocalAuthorizationHandle::open("alpha", &settings).unwrap();
    assert!(
        !model
            .observe_central(
                "CACHE-PRIVATE",
                serde_json::from_value(json!({"status":"Accepted"})).unwrap()
            )
            .unwrap()
    );
    assert_eq!(model.snapshot()["cacheEntries"], 1);
    assert_eq!(model.snapshot()["listEntries"], 1);
    assert!(model.authorize_offline("cache-private", time::OffsetDateTime::now_utc()));
    assert!(
        model
            .offline_start(
                json!({"connectorId":1,"idTag":"Cache-Private","meterStart":600,
        "timestamp":"2026-10-04T00:00:01Z"})
            )
            .unwrap()
    );
    drop(model);
    assert_eq!(
        fs::metadata(&directory.0).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&settings.private_state_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let recovered = LocalAuthorizationHandle::open("alpha", &settings).unwrap();
    assert_eq!(recovered.snapshot()["cacheEntries"], 1);
    assert_eq!(recovered.snapshot()["offlineRecords"], 1);
    assert!(recovered.authorize_offline("CACHE-PRIVATE", time::OffsetDateTime::now_utc()));
    recovered
        .offline_stop(
            1,
            json!({"meterStop":700,"timestamp":"2026-10-04T00:00:02Z"}),
        )
        .unwrap();
    assert_eq!(
        directory.state()["offline"][0]["start"]["idTag"],
        "Cache-Private"
    );
    assert_eq!(directory.state()["offline"][0]["start"]["meterStart"], 600);
    assert_eq!(directory.state()["offline"][0]["stop"]["meterStop"], 700);
    drop(recovered);
    settings.list_supported = true;
    let enabled = LocalAuthorizationHandle::open("alpha", &settings).unwrap();
    assert_eq!(enabled.snapshot()["listVersion"], 7);
    assert_eq!(enabled.snapshot()["listEntries"], 1);
    assert_eq!(enabled.snapshot()["cacheEntries"], 1);
    assert_eq!(enabled.snapshot()["offlineRecords"], 1);
    assert!(!enabled.authorize_offline("CACHE-PRIVATE", time::OffsetDateTime::now_utc()));
    assert!(
        enabled
            .observe_central(
                "cache-private",
                serde_json::from_value(json!({"status":"Accepted"})).unwrap()
            )
            .unwrap()
    );
    assert!(
        !enabled
            .observe_central(
                "cache-private",
                serde_json::from_value(json!({"status":"Blocked"})).unwrap()
            )
            .unwrap()
    );
    drop(enabled);
    settings.list_supported = false;
    let disabled = LocalAuthorizationHandle::open("alpha", &settings).unwrap();
    assert_eq!(disabled.snapshot()["cacheEntries"], 1);
    assert_eq!(disabled.snapshot()["offlineRecords"], 1);
    assert!(disabled.authorize_offline("CACHE-PRIVATE", time::OffsetDateTime::now_utc()));
    assert_eq!(directory.state()["offline"][0]["start"]["meterStart"], 600);
    assert_eq!(directory.state()["offline"][0]["stop"]["meterStop"], 700);
}

fn step(id: &str, action: &str, fields: &str) -> String {
    format!(
        "\n[[steps]]\nid='{id}'\nstation='alpha'\naction='{action}'\ntimeout_ms=5000\n{fields}\n"
    )
}
fn online_prefix(token: &str) -> String {
    let mut scenario = "schema_version=1\nseed=111\n".to_owned();
    scenario += &step("connect", "connect", "");
    scenario += &step(
        "boot",
        "boot",
        "fixture_id='sim.ocpp16.boot.accepted'\npayload={chargePointVendor='UOB',chargePointModel='Simulator'}",
    );
    scenario += &step(
        "authorize",
        "authorize",
        &format!(
            "fixture_id='sim.ocpp16.authorize.accepted'\npayload={{idTag='{token}'}}\nexpect_response={{idTagInfo={{status='Accepted'}}}}"
        ),
    );
    scenario
}
fn configuration(
    directory: &PrivateDirectory,
    address: std::net::SocketAddr,
    list_supported: bool,
) -> PathBuf {
    directory.write("configuration.toml", &format!(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{address}'\nocpp_version='1.6'\nconnectors=[1,2]\nrequest_timeout_ms=2000\n[stations.local_authorization]\nprivate_state_file='{}'\nlist_supported={list_supported}\ncache_supported=true\n", directory.settings().private_state_file))
}
fn events(output: &Output) -> Vec<Value> {
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes).to_lowercase();
        for marker in ["cache-private", "occupancy-private"] {
            assert!(!text.contains(marker));
        }
    }
    std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn actual_cli_disabled_retained_list_allows_independent_cache() {
    let directory = PrivateDirectory::new();
    retained_blocked_list(&directory);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = configuration(&directory, listener.local_addr().unwrap(), false);
    let mut scenario = online_prefix("cache-private");
    scenario += &step(
        "assert-cache",
        "assert_local_authorization",
        "expect_response={cacheEntries=1,listEntries=1}",
    );
    scenario += &step("offline", "csms_offline", "");
    scenario += &step(
        "offline-start",
        "offline_start",
        "payload={connectorId=1,idTag='CACHE-PRIVATE',meterStart=600,timestamp='2026-10-04T00:00:01Z'}\nexpect_response={accepted=true}",
    );
    scenario += &step(
        "offline-stop",
        "offline_stop",
        "payload={connectorId=1,meterStop=700,timestamp='2026-10-04T00:00:02Z'}\nexpect_response={stopped=true}",
    );
    let scenario = directory.write("scenario.toml", &scenario);
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let authorize = receive(&mut socket).await;
        assert_eq!(authorize[2], "Authorize");
        assert_eq!(authorize[3]["idTag"], "cache-private");
        send(
            &mut socket,
            json!([3,authorize[1],{"idTagInfo":{"status":"Accepted"}}]),
        )
        .await;
        closed(&mut socket).await;
    });
    let output = Process::start(&config, &scenario).finish().await;
    let observed = events(&output);
    timeout(BOUND, peer).await.unwrap().unwrap();
    assert!(
        output.status.success(),
        "disabled list must not suppress native central cache"
    );
    assert_eq!(observed.last().unwrap()["event"], "run_passed");
    let state = directory.state();
    assert_eq!(state["cache"].as_object().unwrap().len(), 1);
    assert_eq!(state["offline"].as_array().unwrap().len(), 1);
    assert_eq!(state["offline"][0]["start"]["meterStart"], 600);
    assert_eq!(state["offline"][0]["stop"]["meterStop"], 700);
    cache_recovery::enabled_list_after_process_restart(&directory).await;
}

#[derive(Clone, Copy)]
enum Occupancy {
    SameConnector,
    FreeConnector,
    ConfirmedStop,
}

#[tokio::test]
async fn actual_cli_offline_start_rejects_online_occupied_connector() {
    occupancy_case(Occupancy::SameConnector).await;
}
#[tokio::test]
async fn actual_cli_offline_start_accepts_distinct_free_connector() {
    occupancy_case(Occupancy::FreeConnector).await;
}
#[tokio::test]
async fn actual_cli_offline_start_accepts_connector_after_confirmed_online_stop() {
    occupancy_case(Occupancy::ConfirmedStop).await;
}

async fn occupancy_case(case: Occupancy) {
    let directory = PrivateDirectory::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = configuration(&directory, listener.local_addr().unwrap(), true);
    let mut scenario = online_prefix("occupancy-private");
    scenario += &step(
        "online-start",
        "start_transaction",
        &format!(
            "fixture_id='sim.ocpp16.transaction.start'\npayload={{connectorId=1,idTag='occupancy-private',meterStart=10,timestamp='2026-10-04T00:00:01Z'}}\nexpect_response={{transactionId={TRANSACTION_ID},idTagInfo={{status='Accepted'}}}}"
        ),
    );
    if matches!(case, Occupancy::ConfirmedStop) {
        scenario += &step(
            "online-stop",
            "stop_transaction",
            &format!(
                "fixture_id='sim.ocpp16.transaction.stop'\npayload={{transactionId={TRANSACTION_ID},meterStop=20,timestamp='2026-10-04T00:00:02Z',reason='Local'}}"
            ),
        );
    }
    scenario += &step("offline", "csms_offline", "");
    let connector = if matches!(case, Occupancy::FreeConnector) {
        2
    } else {
        1
    };
    scenario += &step(
        "offline-start",
        "offline_start",
        &format!(
            "payload={{connectorId={connector},idTag='OCCUPANCY-PRIVATE',meterStart=30,timestamp='2026-10-04T00:00:03Z'}}\nexpect_response={{accepted=true}}"
        ),
    );
    let scenario = directory.write("scenario.toml", &scenario);
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let authorize = receive(&mut socket).await;
        assert_eq!(authorize[2], "Authorize");
        send(
            &mut socket,
            json!([3,authorize[1],{"idTagInfo":{"status":"Accepted"}}]),
        )
        .await;
        let start = receive(&mut socket).await;
        assert_eq!(start[2], "StartTransaction");
        assert_eq!(start[3]["connectorId"], 1);
        assert_eq!(start[3]["idTag"], "occupancy-private");
        send(
            &mut socket,
            json!([3,start[1],{"transactionId":TRANSACTION_ID,"idTagInfo":{"status":"Accepted"}}]),
        )
        .await;
        if matches!(case, Occupancy::ConfirmedStop) {
            let stop = receive(&mut socket).await;
            assert_eq!(stop[2], "StopTransaction");
            assert_eq!(stop[3]["transactionId"], TRANSACTION_ID);
            send(&mut socket, json!([3, stop[1], {}])).await;
        }
        closed(&mut socket).await;
    });
    let output = Process::start(&config, &scenario).finish().await;
    let observed = events(&output);
    timeout(BOUND, peer).await.unwrap().unwrap();
    let state = directory.state();
    if matches!(case, Occupancy::SameConnector) {
        assert!(
            !output.status.success(),
            "a confirmed online transaction must keep its connector occupied after CSMS disconnection"
        );
        assert_eq!(
            observed.last().unwrap()["failure_code"],
            "offline_transaction_active"
        );
        assert_eq!(state["offline"].as_array().unwrap().len(), 0);
    } else {
        assert!(output.status.success());
        assert_eq!(observed.last().unwrap()["event"], "run_passed");
        assert_eq!(state["offline"].as_array().unwrap().len(), 1);
        assert_eq!(state["offline"][0]["start"]["connectorId"], connector);
        assert_eq!(state["offline"][0]["start"]["meterStart"], 30);
        assert_eq!(
            state["offline"][0]["start"]["timestamp"],
            "2026-10-04T00:00:03Z"
        );
    }
}
