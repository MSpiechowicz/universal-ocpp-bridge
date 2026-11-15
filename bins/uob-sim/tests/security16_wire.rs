//! Security-event delivery over a real OCPP-J socket: exact `SecurityEventNotification` frames,
//! guaranteed redelivery after a lost acknowledgement or reconnect, and the fault knobs.
mod security16_support;
#[allow(dead_code)]
#[path = "local_authorization16/socket.rs"]
mod socket;
use futures::StreamExt;
use security16_support::{Socket, boot_call, connect, now, private_directory};
use serde_json::{Value, json};
use socket::{accept, boot, receive, send};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::time::timeout;
use uob_sim::ProtocolClient;

const EVENT: &str = "SecurityEventNotification";
const AT: &str = "2026-10-01T12:00:00Z";

/// Receive the next CALL, which must be this security event, and return its frame.
async fn expect_event(socket: &mut Socket, kind: &str, tech_info: Option<&str>) -> Value {
    let frame = receive(socket).await;
    assert_eq!(frame[0], 2, "{frame}");
    assert_eq!(frame[2], EVENT, "{frame}");
    let mut expected = json!({"type": kind, "timestamp": AT});
    if let Some(info) = tech_info {
        expected["techInfo"] = info.into();
    }
    assert_eq!(frame[3], expected);
    frame
}

async fn ack(socket: &mut Socket, frame: &Value) {
    send(socket, json!([3, frame[1], {}])).await;
}

/// No frame at all arrives within the period.
async fn silent(socket: &mut Socket, period: Duration) {
    assert!(
        timeout(period, socket.next()).await.is_err(),
        "unexpected frame"
    );
}

/// The station finished its Boot exchange, so its registration no longer races the peer.
async fn registered(client: &impl ProtocolClient) {
    timeout(Duration::from_secs(5), async {
        while client.accepted_registration() != Some(true) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

async fn settled(client: &impl ProtocolClient, key: &str, value: u64) {
    timeout(Duration::from_secs(10), async {
        while client.security16().unwrap().snapshot()[key] != value {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn events_are_sent_in_order_one_at_a_time_and_dequeued_by_the_confirmation() {
    let directory = private_directory("security-wire");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, first) = connect(&listener, &directory, "", "", accept(&listener)).await;
    let handle = client.security16().unwrap();
    let peer = async {
        let mut socket = first;
        boot(&mut socket).await;
        // Nothing is sent without an event.
        silent(&mut socket, Duration::from_millis(300)).await;
        handle
            .emit("StartupOfTheDevice", Some(AT), None, now())
            .unwrap();
        handle
            .emit(
                "TamperDetectionActivated",
                Some(AT),
                Some("cover opened"),
                now(),
            )
            .unwrap();
        let first = expect_event(&mut socket, "StartupOfTheDevice", None).await;
        // The second event waits for the first confirmation.
        silent(&mut socket, Duration::from_millis(300)).await;
        assert_eq!(handle.snapshot()["pendingEvents"], 2);
        ack(&mut socket, &first).await;
        let second = expect_event(
            &mut socket,
            "TamperDetectionActivated",
            Some("cover opened"),
        )
        .await;
        ack(&mut socket, &second).await;
        silent(&mut socket, Duration::from_millis(300)).await;
        socket
    };
    let (_socket, booted) = tokio::join!(peer, boot_call(&client));
    assert_eq!(booted["status"], "Accepted");
    settled(&client, "eventsAcknowledged", 2).await;
    let snapshot = handle.snapshot();
    assert_eq!(
        (
            &snapshot["pendingEvents"],
            &snapshot["lastAcknowledgedType"]
        ),
        (&json!(0), &json!("TamperDetectionActivated"))
    );
    // No technical detail beyond the type is exposed by the safe snapshot or the traces.
    assert!(!snapshot.to_string().contains("cover opened"));
    assert!(!format!("{:?}", client.traces()).contains("cover opened"));
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn an_unacknowledged_event_is_resent_after_the_connection_is_lost() {
    let directory = private_directory("security-wire-resend");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, first) = connect(&listener, &directory, "", "", accept(&listener)).await;
    let handle = client.security16().unwrap();
    let peer = async {
        let mut socket = first;
        boot(&mut socket).await;
        handle.emit("ResetOrReboot", Some(AT), None, now()).unwrap();
        // A04.FR.02: the acknowledgement is withheld and the connection dies.
        let lost = expect_event(&mut socket, "ResetOrReboot", None).await;
        registered(&client).await;
        drop(socket);
        let mut socket = accept(&listener).await;
        // The reconnected station registers again before it resends anything.
        boot(&mut socket).await;
        let resent = expect_event(&mut socket, "ResetOrReboot", None).await;
        assert_ne!(lost[1], resent[1], "a resend is a new CALL");
        ack(&mut socket, &resent).await;
        silent(&mut socket, Duration::from_millis(300)).await;
    };
    let ((), booted) = tokio::join!(peer, boot_call(&client));
    assert_eq!(booted["status"], "Accepted");
    settled(&client, "eventsAcknowledged", 1).await;
    assert_eq!(handle.snapshot()["eventsEmitted"], 1);
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn an_event_queued_while_disconnected_is_delivered_after_reconnect() {
    let directory = private_directory("security-wire-offline");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, first) = connect(&listener, &directory, "", "", accept(&listener)).await;
    let handle = client.security16().unwrap();
    let peer = async {
        let mut socket = first;
        boot(&mut socket).await;
        registered(&client).await;
        drop(socket);
        timeout(Duration::from_secs(5), async {
            while client.socket_connected() != Some(false) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        handle
            .emit("MemoryExhaustion", Some(AT), None, now())
            .unwrap();
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let frame = expect_event(&mut socket, "MemoryExhaustion", None).await;
        ack(&mut socket, &frame).await;
    };
    let ((), booted) = tokio::join!(peer, boot_call(&client));
    assert_eq!(booted["status"], "Accepted");
    settled(&client, "eventsAcknowledged", 1).await;
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn duplicate_fault_resends_the_acknowledged_event_identically() {
    let directory = private_directory("security-wire-duplicate");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, first) = connect(
        &listener,
        &directory,
        "duplicate_acknowledged=true",
        "",
        accept(&listener),
    )
    .await;
    let handle = client.security16().unwrap();
    let peer = async {
        let mut socket = first;
        boot(&mut socket).await;
        handle
            .emit("InvalidMessages", Some(AT), Some("bad frame"), now())
            .unwrap();
        let original = expect_event(&mut socket, "InvalidMessages", Some("bad frame")).await;
        ack(&mut socket, &original).await;
        let copy = expect_event(&mut socket, "InvalidMessages", Some("bad frame")).await;
        assert_ne!(original[1], copy[1]);
        ack(&mut socket, &copy).await;
        silent(&mut socket, Duration::from_millis(300)).await;
    };
    let ((), booted) = tokio::join!(peer, boot_call(&client));
    assert_eq!(booted["status"], "Accepted");
    settled(&client, "duplicatesSent", 1).await;
    assert_eq!(handle.snapshot()["eventsEmitted"], 1);
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn nothing_is_sent_before_boot_acceptance_unless_the_fault_knob_is_set() {
    let directory = private_directory("security-wire-before-boot");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, first) = connect(&listener, &directory, "", "", accept(&listener)).await;
    let handle = client.security16().unwrap();
    handle
        .emit("StartupOfTheDevice", Some(AT), None, now())
        .unwrap();
    let peer = async {
        let mut socket = first;
        // The Boot is not answered yet, so the queued event must wait.
        let pending = receive(&mut socket).await;
        assert_eq!(pending[2], "BootNotification");
        silent(&mut socket, Duration::from_millis(600)).await;
        let accepted = json!({"status": "Accepted", "currentTime": AT, "interval": 60});
        send(&mut socket, json!([3, pending[1], accepted])).await;
        let frame = expect_event(&mut socket, "StartupOfTheDevice", None).await;
        ack(&mut socket, &frame).await;
    };
    let ((), booted) = tokio::join!(peer, boot_call(&client));
    assert_eq!(booted["status"], "Accepted");
    settled(&client, "eventsAcknowledged", 1).await;
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();

    let directory = private_directory("security-wire-before-boot-fault");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, mut socket) = connect(
        &listener,
        &directory,
        "send_before_accepted=true",
        "",
        accept(&listener),
    )
    .await;
    let handle = client.security16().unwrap();
    handle
        .emit("StartupOfTheDevice", Some(AT), None, now())
        .unwrap();
    let frame = expect_event(&mut socket, "StartupOfTheDevice", None).await;
    // The CSMS refuses an event from an unregistered station: a final answer, so no retry.
    send(
        &mut socket,
        json!([4, frame[1], "ProtocolError", "not registered", {}]),
    )
    .await;
    settled(&client, "eventsRejected", 1).await;
    assert_eq!(handle.snapshot()["pendingEvents"], 0);
    silent(&mut socket, Duration::from_millis(1300)).await;
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_transient_server_fault_is_retried_until_the_confirmation_arrives() {
    let directory = private_directory("security-wire-transient");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, first) = connect(&listener, &directory, "", "", accept(&listener)).await;
    let handle = client.security16().unwrap();
    handle
        .emit("SecurityLogWasCleared", Some(AT), None, now())
        .unwrap();
    let peer = async {
        let mut socket = first;
        boot(&mut socket).await;
        let failed = expect_event(&mut socket, "SecurityLogWasCleared", None).await;
        send(
            &mut socket,
            json!([4, failed[1], "InternalError", "storage failure", {}]),
        )
        .await;
        let started = Instant::now();
        let retried = expect_event(&mut socket, "SecurityLogWasCleared", None).await;
        assert!(started.elapsed() >= Duration::from_millis(800), "backs off");
        ack(&mut socket, &retried).await;
    };
    let ((), booted) = tokio::join!(peer, boot_call(&client));
    assert_eq!(booted["status"], "Accepted");
    settled(&client, "eventsAcknowledged", 1).await;
    assert_eq!(handle.snapshot()["eventsRejected"], 0);
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}
