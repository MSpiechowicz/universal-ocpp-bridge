use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use uob_sim::local_authorization::{LocalAuthorizationConfig, LocalAuthorizationHandle};
use uob_sim::scenario::parse_configuration;
use uob_sim::{ProtocolClient, SimulatorAction, SimulatorCall, SimulatorProtocolClient};

#[path = "local_authorization16/socket.rs"]
mod socket;
use socket::{accept, boot, closed, receive, send};

const BOUND: Duration = Duration::from_secs(10);
const NATIVE_ID: i64 = i64::MAX - 3;

struct PrivateDirectory(PathBuf);
impl PrivateDirectory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("uob-trigger-recovery-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    fn model(&self) -> LocalAuthorizationHandle {
        let model = LocalAuthorizationHandle::open(
            "native-client",
            &LocalAuthorizationConfig {
                private_state_file: self.0.join("state.json").to_str().unwrap().to_owned(),
                list_supported: true,
                cache_supported: true,
            },
        )
        .unwrap();
        let update = serde_json::from_value(json!({"listVersion":7,"updateType":"Full",
            "localAuthorizationList":[{"idTag":"trigger-private","idTagInfo":{"status":"Accepted"}}]})).unwrap();
        assert_eq!(
            model.update(&update),
            ocpp_client::ocpp_types::v16::common::SendLocalListResponseStatus::Accepted
        );
        model
    }
}
impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    CloseInitial,
    OfflineQueued,
    DeniedProbeDone,
    TriggerProbeDone,
}
#[derive(Debug, PartialEq, Eq)]
enum Event {
    Disconnected,
    BootstrapDenied,
    TriggerDenied,
    Replayed,
}

#[tokio::test]
async fn pending_reconnect_boot_then_accepted_trigger_boot_automatically_replays_original_facts() {
    recovery("Pending").await;
}

#[tokio::test]
async fn rejected_reconnect_boot_then_accepted_trigger_boot_automatically_replays_original_facts() {
    recovery("Rejected").await;
}

async fn recovery(denied: &'static str) {
    let directory = PrivateDirectory::new();
    let model = directory.model();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (commands, command_receiver) = mpsc::channel(1);
    let (events, mut event_receiver) = mpsc::channel(1);
    let peer = tokio::spawn(native_peer(listener, denied, command_receiver, events));
    let source = format!(
        "schema_version=1\n[[stations]]\nid='native-client'\nendpoint='ws://{address}'\nocpp_version='1.6'\nrequest_timeout_ms=2000\nreconnect=true\n"
    );
    let mut config = parse_configuration(&source).unwrap().stations[0].client_config();
    config.local_authorization = Some(model.clone());
    let client = SimulatorProtocolClient::connect(config).await.unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: json!({"chargePointVendor":"UOB","chargePointModel":"Trigger recovery"}),
        })
        .await
        .unwrap();
    commands.send(Command::CloseInitial).await.unwrap();
    expect_event(&mut event_receiver, Event::Disconnected).await;
    assert!(
        model
            .offline_start(json!({"connectorId":1,"idTag":"TRIGGER-PRIVATE",
        "meterStart":600,"timestamp":"2026-10-04T00:00:01Z"}))
            .unwrap()
    );
    model
        .offline_stop(
            1,
            json!({"meterStop":700,"timestamp":"2026-10-04T00:00:02Z"}),
        )
        .unwrap();
    assert_eq!(model.snapshot()["offlineRecords"], 1);
    commands.send(Command::OfflineQueued).await.unwrap();
    expect_event(&mut event_receiver, Event::BootstrapDenied).await;
    assert_denied(&client, &model).await;
    commands.send(Command::DeniedProbeDone).await.unwrap();
    expect_event(&mut event_receiver, Event::TriggerDenied).await;
    assert_denied(&client, &model).await;
    commands.send(Command::TriggerProbeDone).await.unwrap();
    // Only the real current-socket accepted triggered Boot can drain the queue.
    // No explicit Boot, model.replay, or another reconnect occurs from here.
    expect_event(&mut event_receiver, Event::Replayed).await;
    timeout(BOUND, async {
        while model.snapshot()["offlineRecords"] != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("acknowledged original native facts must leave the durable queue");
    assert_eq!(client.accepted_registration(), Some(true));
    assert_eq!(model.snapshot()["uncertainRecords"], 0);
    assert_eq!(
        fs::metadata(directory.0.join("state.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let traces = format!("{:?}", client.traces());
    assert!(!traces.contains("trigger-private"));
    assert!(!traces.contains("TRIGGER-PRIVATE"));
    client.shutdown().await.unwrap();
    timeout(BOUND, peer).await.unwrap().unwrap();
}

async fn assert_denied(client: &SimulatorProtocolClient, model: &LocalAuthorizationHandle) {
    // The same-socket native response follows the denied Boot response in wire
    // order. Any premature Start/Stop instead fails the peer's Heartbeat check.
    client.heartbeat().await.unwrap();
    assert_eq!(client.accepted_registration(), Some(false));
    assert_eq!(model.snapshot()["offlineRecords"], 1);
    assert_eq!(model.snapshot()["uncertainRecords"], 0);
}

async fn expect_event(receiver: &mut mpsc::Receiver<Event>, expected: Event) {
    assert_eq!(
        timeout(BOUND, receiver.recv()).await.unwrap(),
        Some(expected)
    );
}
async fn expect_command(receiver: &mut mpsc::Receiver<Command>, expected: Command) {
    assert_eq!(
        timeout(BOUND, receiver.recv()).await.unwrap(),
        Some(expected)
    );
}

async fn native_peer(
    listener: TcpListener,
    denied: &'static str,
    mut commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Event>,
) {
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    expect_command(&mut commands, Command::CloseInitial).await;
    socket.close(None).await.unwrap();
    drop(socket);
    let mut socket = accept(&listener).await;
    let bootstrap = receive(&mut socket).await;
    assert_eq!(bootstrap[2], "BootNotification");
    events.send(Event::Disconnected).await.unwrap();
    expect_command(&mut commands, Command::OfflineQueued).await;
    reply_boot(&mut socket, &bootstrap, denied).await;
    events.send(Event::BootstrapDenied).await.unwrap();
    heartbeat(&mut socket).await;
    expect_command(&mut commands, Command::DeniedProbeDone).await;
    trigger_boot(&mut socket, "denied-trigger", denied).await;
    events.send(Event::TriggerDenied).await.unwrap();
    heartbeat(&mut socket).await;
    expect_command(&mut commands, Command::TriggerProbeDone).await;
    trigger_boot(&mut socket, "accepted-trigger", "Accepted").await;
    replay_original_facts(&mut socket).await;
    events.send(Event::Replayed).await.unwrap();
    closed(&mut socket).await;
}

async fn heartbeat(socket: &mut WebSocketStream<TcpStream>) {
    let frame = receive(socket).await;
    assert_eq!(
        frame[2], "Heartbeat",
        "denied registration must not send native offline facts"
    );
    send(
        socket,
        json!([3,frame[1],{"currentTime":"2026-10-04T00:00:00Z"}]),
    )
    .await;
}

async fn trigger_boot(socket: &mut WebSocketStream<TcpStream>, id: &str, status: &str) {
    send(
        socket,
        json!([2,id,"TriggerMessage",{"requestedMessage":"BootNotification"}]),
    )
    .await;
    assert_eq!(receive(socket).await, json!([3,id,{"status":"Accepted"}]));
    let request = receive(socket).await;
    assert_eq!(request[2], "BootNotification");
    reply_boot(socket, &request, status).await;
}

async fn reply_boot(socket: &mut WebSocketStream<TcpStream>, request: &Value, status: &str) {
    send(
        socket,
        json!([3,request[1],{"status":status,"currentTime":"2026-10-04T00:00:00Z","interval":60}]),
    )
    .await;
}

async fn replay_original_facts(socket: &mut WebSocketStream<TcpStream>) {
    let start = receive(socket).await;
    assert_eq!(
        start[2], "StartTransaction",
        "accepted triggered registration must automatically replay"
    );
    assert_eq!(start[3]["connectorId"], 1);
    assert_eq!(start[3]["idTag"], "TRIGGER-PRIVATE");
    assert_eq!(start[3]["meterStart"], 600);
    assert_eq!(start[3]["timestamp"], "2026-10-04T00:00:01Z");
    send(
        socket,
        json!([3,start[1],{"transactionId":NATIVE_ID,"idTagInfo":{"status":"Accepted"}}]),
    )
    .await;
    let stop = receive(socket).await;
    assert_eq!(stop[2], "StopTransaction");
    assert_eq!(stop[3]["transactionId"], NATIVE_ID);
    assert_eq!(stop[3]["idTag"], "TRIGGER-PRIVATE");
    assert_eq!(stop[3]["meterStop"], 700);
    assert_eq!(stop[3]["timestamp"], "2026-10-04T00:00:02Z");
    assert_eq!(stop[3]["reason"], "Local");
    send(socket, json!([3, stop[1], {}])).await;
}
