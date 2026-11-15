//! `NotifyEvent` over the socket: thresholds, bursts, levels and the CSMS's answers.
use crate::monitoring201_support::{connector, item};
use crate::socket::{accept, boot, receive, send};
use crate::{call, expect_call, quiet, run, station};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use uob_sim::{ProtocolClient, SimulatorProtocolClient};

fn temperature() -> Value {
    json!({"name": "Temperature"})
}

/// Accept, boot and monitor the connector temperature; the station side waits for `go`.
async fn monitor_temperature(listener: &TcpListener, go: &UnboundedSender<()>) -> crate::Socket {
    let mut socket = accept(listener).await;
    boot(&mut socket).await;
    let request = json!({"setMonitoringData": [item(&connector(), "Temperature", "UpperThreshold", 75.0, 3)]});
    let reply = call(&mut socket, "set", "SetVariableMonitoring", request).await;
    assert_eq!(reply[2]["setMonitoringResult"][0]["id"], 4031);
    go.send(()).unwrap();
    socket
}

async fn station_side(
    listener: &TcpListener,
    fields: Value,
    mut go: UnboundedReceiver<()>,
    act: impl AsyncFn(&SimulatorProtocolClient, &mut UnboundedReceiver<()>),
) -> SimulatorProtocolClient {
    let client = station(listener, fields).await;
    act(&client, &mut go).await;
    client
}

#[tokio::test]
async fn threshold_events_are_sent_alerting_then_cleared_and_acknowledged() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (go, wait) = unbounded_channel();
    let peer = async {
        let mut socket = monitor_temperature(&listener, &go).await;
        let alert = expect_call(&mut socket, "NotifyEvent").await;
        let cleared = expect_call(&mut socket, "NotifyEvent").await;
        for message in [&alert, &cleared] {
            assert_eq!(
                (
                    &message["seqNo"],
                    &message["eventData"][0]["variableMonitoringId"]
                ),
                (&json!(0), &json!(4031))
            );
            assert_eq!(message["eventData"][0]["component"], connector());
            assert_eq!(message["eventData"][0]["variable"], temperature());
        }
        assert_eq!(alert["eventData"][0]["trigger"], "Alerting");
        assert_eq!(alert["eventData"][0].get("cleared"), None);
        assert_eq!(
            (
                &cleared["eventData"][0]["trigger"],
                &cleared["eventData"][0]["cleared"]
            ),
            (&json!("Alerting"), &json!(true))
        );
        assert_eq!(
            alert["eventData"][0]["eventNotificationType"],
            "CustomMonitor"
        );
        assert!(quiet(&mut socket).await);
    };
    let client = station_side(&listener, json!({}), wait, async |client, go| {
        go.recv().await.unwrap();
        for value in ["80", "70"] {
            client
                .monitoring201()
                .unwrap()
                .set_value(&connector(), &temperature(), value)
                .unwrap();
        }
    });
    let client = run(peer, client).await;
    let snapshot = client.monitoring201().unwrap().snapshot();
    assert_eq!(
        (
            &snapshot["eventsAcknowledged"],
            &snapshot["lastEventOutcome"]
        ),
        (&json!(2), &json!("Result"))
    );
    client.abort();
}

#[tokio::test]
async fn a_burst_is_chained_with_tbc_and_a_call_error_is_recorded() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (go, wait) = unbounded_channel();
    let peer = async {
        let mut socket = monitor_temperature(&listener, &go).await;
        let mut shape = Vec::new();
        for _ in 0..3 {
            let message = expect_call(&mut socket, "NotifyEvent").await;
            shape.push((
                message["seqNo"].as_i64().unwrap(),
                message["tbc"].as_bool().unwrap(),
                message["eventData"].as_array().unwrap().len(),
            ));
        }
        assert_eq!(shape, [(0, true, 2), (1, true, 2), (2, false, 1)]);
        // A CALLERROR is received but unwanted: it is recorded and never resent.
        let frame = receive(&mut socket).await;
        assert_eq!(frame[2], "NotifyEvent");
        send(&mut socket, json!([4, frame[1], "InternalError", "no", {}])).await;
        assert!(quiet(&mut socket).await, "a CALLERROR is not retried");
    };
    let client = station_side(
        &listener,
        json!({"events_per_message": 2}),
        wait,
        async |client, go| {
            go.recv().await.unwrap();
            let monitoring = client.monitoring201().unwrap();
            monitoring
                .emit_events(&connector(), &temperature(), 5, Some(4031))
                .unwrap();
            monitoring
                .emit_events(&connector(), &temperature(), 1, Some(4031))
                .unwrap();
        },
    );
    let client = run(peer, client).await;
    let snapshot = client.monitoring201().unwrap().snapshot();
    assert_eq!(
        (
            &snapshot["eventsAcknowledged"],
            &snapshot["eventsRejected"],
            &snapshot["lastEventOutcome"],
            &snapshot["eventsQueued"]
        ),
        (&json!(3), &json!(1), &json!("CallError"), &json!(0))
    );
    client.abort();
}

#[tokio::test]
async fn events_above_the_level_set_by_the_csms_are_not_sent() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (go, wait) = unbounded_channel();
    let peer = async {
        let mut socket = monitor_temperature(&listener, &go).await;
        let level = call(
            &mut socket,
            "level",
            "SetMonitoringLevel",
            json!({"severity": 2}),
        )
        .await;
        assert_eq!(level[2], json!({"status": "Accepted"}));
        go.send(()).unwrap();
        assert!(
            quiet(&mut socket).await,
            "N07.FR.15: a severity 3 monitor is above level 2"
        );
        let level = call(
            &mut socket,
            "level2",
            "SetMonitoringLevel",
            json!({"severity": 3}),
        )
        .await;
        assert_eq!(level[2]["status"], "Accepted");
        go.send(()).unwrap();
        let event = expect_call(&mut socket, "NotifyEvent").await;
        assert_eq!(event["eventData"][0]["cleared"], true);
    };
    let client = station_side(&listener, json!({}), wait, async |client, go| {
        let monitoring = client.monitoring201().unwrap();
        go.recv().await.unwrap();
        // Level 2 is not set yet: wait for the CSMS's restriction before the value moves.
        go.recv().await.unwrap();
        monitoring
            .set_value(&connector(), &temperature(), "80")
            .unwrap();
        go.recv().await.unwrap();
        monitoring
            .set_value(&connector(), &temperature(), "70")
            .unwrap();
    });
    let client = run(peer, client).await;
    assert_eq!(
        client.monitoring201().unwrap().snapshot()["eventsSuppressed"],
        1
    );
    client.abort();
}
