use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::{Instant, timeout};
use uob_sim::local_authorization::transport::NativeReplyFault;
use uob_sim::scenario::parse_configuration;
use uob_sim::{ProtocolClient, SimulatorAction, SimulatorCall, SimulatorProtocolClient};

#[path = "local_authorization16/socket.rs"]
mod socket;
use socket::{accept, boot, closed, receive, send};

const BOUND: Duration = Duration::from_secs(5);

struct PrivateDirectory(PathBuf);
impl PrivateDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("uob-native-wire-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    fn configuration(&self, address: std::net::SocketAddr) -> String {
        format!(
            "schema_version=1\n[[stations]]\nid='wire-station'\nendpoint='ws://{address}'\nocpp_version='1.6'\n[stations.local_authorization]\nprivate_state_file='{}'\n",
            self.0.join("state.json").display()
        )
    }
}
impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn native_conflicts_report_connector_zero_and_listed_tokens_never_enter_cache() {
    let directory = PrivateDirectory::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (ready, installed) = oneshot::channel();
    let (cached, cache_observed) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        send(&mut socket, json!([2,"blocked-list","SendLocalList",{
            "listVersion":1,"updateType":"Full","localAuthorizationList":[{
                "idTag":"private-conflict","idTagInfo":{"status":"Blocked","parentIdTag":"private-parent"}
            }]
        }])).await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        ready.send(()).unwrap();
        let authorize = receive(&mut socket).await;
        assert_eq!(authorize[2], "Authorize");
        send(&mut socket, json!([3,authorize[1],{"idTagInfo":{"status":"Accepted","parentIdTag":"private-parent"}}])).await;
        let mut notified = false;
        let mut cached_identity = false;
        while !notified || !cached_identity {
            let frame = receive(&mut socket).await;
            if frame[2] == "StatusNotification" {
                assert_eq!(frame[3]["connectorId"], 0);
                assert_eq!(frame[3]["errorCode"], "LocalListConflict");
                assert!(frame[3].get("idTag").is_none());
                notified = true;
                send(&mut socket, json!([3, frame[1], {}])).await;
            } else {
                assert_eq!(frame[2], "Authorize");
                cached_identity = true;
                send(
                    &mut socket,
                    json!([3,frame[1],{"idTagInfo":{"status":"Accepted"}}]),
                )
                .await;
            }
        }
        timeout(BOUND, cache_observed).await.unwrap().unwrap();
        send(&mut socket, json!([2, "clear-cache", "ClearCache", {}])).await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        closed(&mut socket).await;
    });
    let source = directory.configuration(address);
    let client = SimulatorProtocolClient::connect(
        parse_configuration(&source).unwrap().stations[0].client_config(),
    )
    .await
    .unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: json!({"chargePointVendor":"UOB","chargePointModel":"Simulator"}),
        })
        .await
        .unwrap();
    timeout(BOUND, installed).await.unwrap().unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::Authorize,
            payload: json!({"idTag":"PRIVATE-CONFLICT"}),
        })
        .await
        .unwrap();
    let local = client.local_authorization().unwrap();
    assert_eq!(local.snapshot()["cacheEntries"], 0);
    assert!(!local.authorize_offline("private-conflict", time::OffsetDateTime::now_utc()));
    client
        .call(SimulatorCall {
            action: SimulatorAction::Authorize,
            payload: json!({"idTag":"private-cache"}),
        })
        .await
        .unwrap();
    assert_eq!(local.snapshot()["cacheEntries"], 1);
    cached.send(()).unwrap();
    timeout(BOUND, async {
        while local.snapshot()["cacheEntries"] != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(local.snapshot()["listEntries"], 1);
    let public = format!("{:?}", client.traces()).to_lowercase();
    for secret in ["private-conflict", "private-parent", "private-cache"] {
        assert!(!public.contains(secret));
    }
    client.shutdown().await.unwrap();
    timeout(BOUND, peer).await.unwrap().unwrap();
}

#[tokio::test]
async fn native_unicode_update_dedup_delay_and_lost_reply_commit_before_socket_fault() {
    let directory = PrivateDirectory::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (connected, start_peer) = oneshot::channel();
    let (delay_done, delay_observed) = oneshot::channel();
    let (drop_armed, drop_ready) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        start_peer.await.unwrap();
        boot(&mut socket).await;
        let update = json!([2,"unicode-list","SendLocalList",{
            "listVersion":1,"updateType":"Full","localAuthorizationList":[{
                "idTag":"界界界界界界界界界界界界界界界界界界界界","idTagInfo":{"status":"Accepted","parentIdTag":"父".repeat(20)}
            }]
        }]);
        let started = Instant::now();
        send(&mut socket, update.clone()).await;
        assert_eq!(
            receive(&mut socket).await,
            json!([3,"unicode-list",{"status":"Accepted"}])
        );
        assert!(started.elapsed() >= Duration::from_millis(100));
        send(&mut socket, update).await;
        assert_eq!(
            receive(&mut socket).await,
            json!([3,"unicode-list",{"status":"Accepted"}])
        );
        send(
            &mut socket,
            json!([2,"unicode-list","SendLocalList",{"listVersion":2,"updateType":"Full"}]),
        )
        .await;
        assert_eq!(receive(&mut socket).await[0], 4);
        delay_done.send(()).unwrap();
        let authorize = receive(&mut socket).await;
        assert_eq!(authorize[2], "Authorize");
        assert_eq!(authorize[3]["idTag"], "界".repeat(20));
        send(&mut socket, json!([3,authorize[1],{"idTagInfo":{"status":"Accepted","parentIdTag":"父".repeat(20)}}])).await;
        drop_ready.await.unwrap();
        send(
            &mut socket,
            json!([2,"lost-full","SendLocalList",{"listVersion":2,"updateType":"Full"}]),
        )
        .await;
        // The next native mutation was committed, but no CALLRESULT may be fabricated.
        closed(&mut socket).await;
        let _ = socket.close(None).await;
    });
    let source = directory.configuration(address);
    let client = SimulatorProtocolClient::connect(
        parse_configuration(&source).unwrap().stations[0].client_config(),
    )
    .await
    .unwrap();
    client
        .arm_local_reply_fault(NativeReplyFault::Delay(Duration::from_millis(120)))
        .unwrap();
    connected.send(()).unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: json!({"chargePointVendor":"UOB","chargePointModel":"Simulator"}),
        })
        .await
        .unwrap();
    timeout(BOUND, delay_observed).await.unwrap().unwrap();
    let local = client.local_authorization().unwrap();
    assert_eq!(local.snapshot()["listEntries"], 1);
    let response = client
        .call(SimulatorCall {
            action: SimulatorAction::Authorize,
            payload: json!({"idTag":"界".repeat(20)}),
        })
        .await
        .unwrap();
    assert_eq!(response["idTagInfo"]["status"], "Accepted");
    assert_eq!(response["idTagInfo"]["parentIdTag"], "父".repeat(20));
    assert_eq!(local.snapshot()["cacheEntries"], 0);
    let trace = format!("{:?}", client.traces());
    assert!(!trace.contains(&"界".repeat(20)));
    assert!(!trace.contains(&"父".repeat(20)));
    client
        .arm_local_reply_fault(NativeReplyFault::DropConnection)
        .unwrap();
    drop_armed.send(()).unwrap();
    timeout(BOUND, peer).await.unwrap().unwrap();
    assert_eq!(local.snapshot()["listEntries"], 0);
    assert_eq!(local.snapshot()["listVersion"], 0);
    client.shutdown().await.unwrap();
}
