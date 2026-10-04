use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;
use uob_sim::local_authorization::{LocalAuthorizationConfig, LocalAuthorizationHandle};
use uob_sim::scenario::parse_configuration;
use uob_sim::{ProtocolClient, SimulatorAction, SimulatorCall, SimulatorProtocolClient};

const BOUND: Duration = Duration::from_secs(5);
const FULL: &str =
    include_str!("../../../tests/ocpp-fixtures/corpus/wire/1.6/local-list-full.json");
#[path = "local_authorization16/socket.rs"]
mod socket;
use socket::{accept, boot, closed, receive, send};

struct PrivateDirectory(PathBuf);
impl PrivateDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("uob-native-client-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    fn model(&self) -> LocalAuthorizationHandle {
        LocalAuthorizationHandle::open(
            "native-client",
            &LocalAuthorizationConfig {
                private_state_file: self.0.join("state.json").to_str().unwrap().to_owned(),
                list_supported: true,
                cache_supported: true,
            },
        )
        .unwrap()
    }
}
impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn real_socket_offline_replay_mapping_reset_disk_recovery_and_private_output() {
    let directory = PrivateDirectory::new();
    let model = directory.model();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (updated, update_done) = oneshot::channel();
    let (closed_sender, socket_closed) = oneshot::channel();
    let (rebooted, reboot_done) = oneshot::channel();
    let (allow_reset, reset_allowed) = oneshot::channel();
    let peer = tokio::spawn(replay_reset_peer(
        listener,
        updated,
        closed_sender,
        rebooted,
        reset_allowed,
    ));
    let source = format!(
        "schema_version=1\n[[stations]]\nid='native-client'\nendpoint='ws://{address}'\nocpp_version='1.6'\nrequest_timeout_ms=2000\n"
    );
    let mut config = parse_configuration(&source).unwrap().stations[0].client_config();
    config.local_authorization = Some(model.clone());
    let client = SimulatorProtocolClient::connect(config.clone())
        .await
        .unwrap();
    let boot_payload = json!({"chargePointVendor":"UOB","chargePointModel":"Private station"});
    client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: boot_payload.clone(),
        })
        .await
        .unwrap();
    timeout(BOUND, update_done).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
    timeout(BOUND, socket_closed).await.unwrap().unwrap();
    assert!(!model.offline_start(json!({"connectorId":1,"idTag":"unknown-private","meterStart":0,"timestamp":"2026-10-04T00:00:01Z"})).unwrap());
    assert!(model.offline_start(json!({"connectorId":1,"idTag":"list-marker-111","meterStart":1,"timestamp":"2026-10-04T00:00:01Z"})).unwrap());
    model
        .offline_stop(1, json!({"meterStop":2,"timestamp":"2026-10-04T00:00:02Z"}))
        .unwrap();
    assert_eq!(model.snapshot()["offlineRecords"], 1);
    let client = SimulatorProtocolClient::connect(config).await.unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: boot_payload,
        })
        .await
        .unwrap();
    model.replay(&client).await.unwrap();
    allow_reset.send(()).unwrap();
    timeout(BOUND, reboot_done).await.unwrap().unwrap();
    timeout(BOUND, async {
        while client.reboot_count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(model.snapshot()["offlineRecords"], 0);
    assert_eq!(model.snapshot()["cacheEntries"], 0);
    assert!(model.authorize_offline("LIST-MARKER-111", time::OffsetDateTime::now_utc()));
    let trace = format!("{:?}", client.traces());
    for secret in ["list-marker-111", "LIST-MARKER-111", "PARENT-MARKER-111"] {
        assert!(!trace.contains(secret));
    }
    client.shutdown().await.unwrap();
    timeout(BOUND, peer).await.unwrap().unwrap();
}

#[tokio::test]
async fn uncertain_native_replay_is_durable_and_not_retransmitted_on_new_socket() {
    let directory = PrivateDirectory::new();
    let model = directory.model();
    let update = serde_json::from_value(
        json!({"listVersion":1,"updateType":"Full","localAuthorizationList":[{
            "idTag":"private-uncertain","idTagInfo":{"status":"Accepted"}
        }]}),
    )
    .unwrap();
    assert_eq!(
        model.update(&update),
        ocpp_client::ocpp_types::v16::common::SendLocalListResponseStatus::Accepted
    );
    model.offline_start(json!({"connectorId":1,"idTag":"private-uncertain","meterStart":1,"timestamp":"2026-10-04T00:00:00Z"})).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (start_lost, lost_start_observed) = oneshot::channel();
    let (registered_again, registration_observed) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        accepted_triggered_boot(&mut socket, "uncertain-boot").await;
        let start = receive(&mut socket).await;
        assert_eq!(start[2], "StartTransaction");
        assert_eq!(start[3]["idTag"], "private-uncertain");
        assert_eq!(start[3]["meterStart"], 1);
        assert_eq!(start[3]["timestamp"], "2026-10-04T00:00:00Z");
        socket.close(None).await.unwrap();
        // The native Start actually reached the CSMS, which deliberately sent
        // no response before closing. A queued/busy error is not this proof.
        start_lost.send(()).unwrap();
        let mut socket = accept(&listener).await;
        accepted_triggered_boot(&mut socket, "recovered-uncertain-boot").await;
        registered_again.send(()).unwrap();
        // Only the explicit Heartbeat may appear: the uncertain Start must not replay.
        let heartbeat = receive(&mut socket).await;
        assert_eq!(heartbeat[2], "Heartbeat");
        send(
            &mut socket,
            json!([3,heartbeat[1],{"currentTime":"2026-10-04T00:00:00Z"}]),
        )
        .await;
        closed(&mut socket).await;
    });
    let source = format!(
        "schema_version=1\n[[stations]]\nid='native-client'\nendpoint='ws://{address}'\nocpp_version='1.6'\nrequest_timeout_ms=100\n"
    );
    let mut config = parse_configuration(&source).unwrap().stations[0].client_config();
    config.local_authorization = Some(model.clone());
    let client = SimulatorProtocolClient::connect(config.clone())
        .await
        .unwrap();
    timeout(BOUND, lost_start_observed).await.unwrap().unwrap();
    // Accepted triggered Boot starts replay automatically. Observe its durable
    // unknown execution outcome instead of racing it with explicit replay.
    timeout(BOUND, async {
        while model.snapshot()["uncertainRecords"] != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("lost native Start reply must become durably uncertain");
    assert_eq!(model.snapshot()["uncertainRecords"], 1);
    client.shutdown().await.unwrap();
    drop(client);
    drop(config);
    drop(model);
    let recovered = directory.model();
    assert_eq!(recovered.snapshot()["uncertainRecords"], 1);
    let mut config = parse_configuration(&source).unwrap().stations[0].client_config();
    config.local_authorization = Some(recovered.clone());
    let client = SimulatorProtocolClient::connect(config).await.unwrap();
    timeout(BOUND, registration_observed)
        .await
        .unwrap()
        .unwrap();
    client.heartbeat().await.unwrap();
    assert_eq!(client.accepted_registration(), Some(true));
    // This probe offers the recovered uncertain-only queue to a registered
    // native client; neither automatic nor explicit replay may retransmit it.
    recovered.replay(&client).await.unwrap();
    assert_eq!(recovered.snapshot()["uncertainRecords"], 1);
    client.shutdown().await.unwrap();
    timeout(BOUND, peer).await.unwrap().unwrap();
}

async fn accepted_triggered_boot(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    id: &str,
) {
    send(
        socket,
        json!([2,id,"TriggerMessage",{"requestedMessage":"BootNotification"}]),
    )
    .await;
    assert_eq!(receive(socket).await, json!([3,id,{"status":"Accepted"}]));
    boot(socket).await;
}

#[tokio::test]
async fn replayed_central_denial_stops_with_the_recovered_native_id() {
    ongoing_transaction(true).await;
}

#[tokio::test]
async fn actual_hard_reset_stops_ongoing_replayed_charging_after_disk_recovery() {
    ongoing_transaction(false).await;
}

async fn ongoing_transaction(central_denial: bool) {
    let directory = PrivateDirectory::new();
    let model = directory.model();
    prepare_ongoing(&model);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (reset, allow_reset) = oneshot::channel();
    let (stopped, stop_observed) = oneshot::channel();
    let peer = tokio::spawn(ongoing_peer(listener, central_denial, allow_reset, stopped));
    let source = format!(
        "schema_version=1\n[[stations]]\nid='native-client'\nendpoint='ws://{address}'\nocpp_version='1.6'\n"
    );
    let mut config = parse_configuration(&source).unwrap().stations[0].client_config();
    config.local_authorization = Some(model.clone());
    let client = SimulatorProtocolClient::connect(config).await.unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: json!({"chargePointVendor":"UOB","chargePointModel":"Simulator"}),
        })
        .await
        .unwrap();
    model.replay(&client).await.unwrap();
    if !central_denial {
        reset.send(()).unwrap();
    }
    timeout(BOUND, stop_observed).await.unwrap().unwrap();
    timeout(BOUND, async {
        while model.snapshot()["offlineRecords"] != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(model.snapshot()["listEntries"], 1);
    assert_eq!(model.snapshot()["cacheEntries"], 0);
    client.shutdown().await.unwrap();
    timeout(BOUND, peer).await.unwrap().unwrap();
}

async fn replay_reset_peer(
    listener: TcpListener,
    updated: oneshot::Sender<()>,
    closed_sender: oneshot::Sender<()>,
    rebooted: oneshot::Sender<()>,
    reset_allowed: oneshot::Receiver<()>,
) {
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    let mut full: Value = serde_json::from_str(FULL).unwrap();
    full[1] = json!("native-full");
    send(&mut socket, full).await;
    assert_eq!(
        receive(&mut socket).await,
        json!([3,"native-full",{"status":"Accepted"}])
    );
    send(
        &mut socket,
        json!([2, "native-version", "GetLocalListVersion", {}]),
    )
    .await;
    assert_eq!(
        receive(&mut socket).await,
        json!([3,"native-version",{"listVersion":1}])
    );
    send(&mut socket, json!([2, "native-clear", "ClearCache", {}])).await;
    assert_eq!(
        receive(&mut socket).await,
        json!([3,"native-clear",{"status":"Accepted"}])
    );
    updated.send(()).unwrap();
    closed(&mut socket).await;
    closed_sender.send(()).unwrap();
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    let start = receive(&mut socket).await;
    assert_eq!(start[2], "StartTransaction");
    assert_eq!(start[3]["idTag"], "list-marker-111");
    assert_eq!(start[3]["timestamp"], "2026-10-04T00:00:01Z");
    send(&mut socket, json!([3,start[1],{"transactionId":37,"idTagInfo":{"status":"Accepted","parentIdTag":"PARENT-MARKER-111"}}])).await;
    let stop = receive(&mut socket).await;
    assert_eq!(stop[2], "StopTransaction");
    assert_eq!(stop[3]["transactionId"], 37);
    assert_eq!(stop[3]["timestamp"], "2026-10-04T00:00:02Z");
    send(&mut socket, json!([3, stop[1], {}])).await;
    timeout(BOUND, reset_allowed).await.unwrap().unwrap();
    send(
        &mut socket,
        json!([2,"native-reset","Reset",{"type":"Soft"}]),
    )
    .await;
    assert_eq!(
        receive(&mut socket).await,
        json!([3,"native-reset",{"status":"Accepted"}])
    );
    closed(&mut socket).await;
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    send(
        &mut socket,
        json!([2, "post-reset-version", "GetLocalListVersion", {}]),
    )
    .await;
    assert_eq!(
        receive(&mut socket).await,
        json!([3,"post-reset-version",{"listVersion":1}])
    );
    rebooted.send(()).unwrap();
    closed(&mut socket).await;
}

fn prepare_ongoing(model: &LocalAuthorizationHandle) {
    let update = serde_json::from_value(
        json!({"listVersion":1,"updateType":"Full","localAuthorizationList":[{
            "idTag":"private-active","idTagInfo":{"status":"Accepted","parentIdTag":"private-parent"}
        }]}),
    )
    .unwrap();
    assert_eq!(
        model.update(&update),
        ocpp_client::ocpp_types::v16::common::SendLocalListResponseStatus::Accepted
    );
    assert!(
        model
            .offline_start(json!({"connectorId":1,"idTag":"PRIVATE-ACTIVE",
        "meterStart":i64::MAX - 2,"timestamp":"2026-09-01T00:00:00Z"}))
            .unwrap()
    );
}

async fn ongoing_peer(
    listener: TcpListener,
    central_denial: bool,
    allow_reset: oneshot::Receiver<()>,
    stopped: oneshot::Sender<()>,
) {
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    let start = receive(&mut socket).await;
    assert_eq!(start[2], "StartTransaction");
    assert_eq!(start[3]["idTag"], "PRIVATE-ACTIVE");
    assert_eq!(start[3]["meterStart"], i64::MAX - 2);
    send(
        &mut socket,
        json!([3,start[1],{"transactionId":i64::MAX - 3,
        "idTagInfo":{"status":if central_denial { "Blocked" } else { "Accepted" }}}]),
    )
    .await;
    if central_denial {
        let mut notified = false;
        let mut stopped_transaction = false;
        while !notified || !stopped_transaction {
            let frame = receive(&mut socket).await;
            if frame[2] == "StatusNotification" {
                assert_eq!(frame[3]["connectorId"], 0);
                assert_eq!(frame[3]["errorCode"], "LocalListConflict");
                notified = true;
            } else {
                assert_native_stop(&frame, "DeAuthorized");
                stopped_transaction = true;
            }
            send(&mut socket, json!([3, frame[1], {}])).await;
        }
    } else {
        let conflict = receive(&mut socket).await;
        assert_eq!(conflict[2], "StatusNotification");
        assert_eq!(conflict[3]["errorCode"], "LocalListConflict");
        // The real auxiliary ACK remains outstanding across Reset. Known Boot
        // and Start replies must not depend on its completion.
        timeout(BOUND, allow_reset).await.unwrap().unwrap();
        send(
            &mut socket,
            json!([2,"ongoing-reset","Reset",{"type":"Hard"}]),
        )
        .await;
        assert_eq!(
            receive(&mut socket).await,
            json!([3,"ongoing-reset",{"status":"Accepted"}])
        );
        closed(&mut socket).await;
        socket = accept(&listener).await;
        boot(&mut socket).await;
        send(
            &mut socket,
            json!([2, "recovery-version", "GetLocalListVersion", {}]),
        )
        .await;
        let mut queried = false;
        let mut stopped_transaction = false;
        while !queried || !stopped_transaction {
            let frame = receive(&mut socket).await;
            if frame[0] == 3 {
                assert_eq!(frame, json!([3,"recovery-version",{"listVersion":1}]));
                queried = true;
            } else {
                assert_native_stop(&frame, "HardReset");
                stopped_transaction = true;
                send(&mut socket, json!([3, frame[1], {}])).await;
            }
        }
    }
    stopped.send(()).unwrap();
    closed(&mut socket).await;
}

fn assert_native_stop(frame: &Value, reason: &str) {
    assert_eq!(frame[2], "StopTransaction");
    assert_eq!(frame[3]["transactionId"], i64::MAX - 3);
    assert_eq!(frame[3]["meterStop"], i64::MAX - 2);
    assert_eq!(frame[3]["idTag"], "PRIVATE-ACTIVE");
    assert_eq!(frame[3]["reason"], reason);
}
