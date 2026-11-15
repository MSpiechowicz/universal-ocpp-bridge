//! The real 2.0.1 simulator process runs monitoring scenarios against a scripted CSMS: setup,
//! report fragments, event bursts, a failing assertion and a report cut short by a closed socket.
#[allow(dead_code)] // Shared helpers also serve the local-authorization process targets.
#[path = "local_authorization201/process.rs"]
mod common;
mod monitoring201_support;
use common::{
    PrivateDirectory, Process, accept, boot, closed, native_boot, prefix, receive, send, step,
};
use monitoring201_support::{connector, document, evse, item, station_component};
use serde_json::{Value, json};
use tokio::net::TcpListener;

type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

async fn call(socket: &mut Socket, id: &str, action: &str, payload: Value) -> Value {
    send(socket, json!([2, id, action, payload])).await;
    receive(socket).await
}

/// Acknowledge the next station CALL, which must be this action; returns its payload.
async fn expect(socket: &mut Socket, action: &str) -> Value {
    let frame = receive(socket).await;
    assert_eq!(frame[2], action, "{frame}");
    send(socket, json!([3, frame[1], {}])).await;
    frame[3].clone()
}

fn start(
    directory: &PrivateDirectory,
    listener: &TcpListener,
    fields: &Value,
    steps: &str,
) -> Process {
    let endpoint = format!("ws://{}/ocpp/alpha", listener.local_addr().unwrap());
    let config = directory.write("config.toml", &document(&endpoint, fields.clone()));
    let scenario = directory.write(
        "scenario.toml",
        &(prefix() + &step("connect", "connect", "") + &native_boot("boot", "Accepted") + steps),
    );
    Process::start(&config, &scenario)
}

fn payload(extra: &str) -> String {
    format!(
        "payload={{component={{name='Connector',evse={{id=1,connectorId=1}}}},variable={{name='Temperature'}},{extra}}}"
    )
}

#[tokio::test]
async fn a_scenario_follows_setup_report_events_and_removal() {
    let directory = PrivateDirectory::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let steps = step(
        "monitor",
        "await_monitoring",
        "expect_response={monitorIds=[4003,4030,4031]}",
    ) + &step(
        "report",
        "await_monitoring",
        "expect_response={reportsDelivered=1,reportFragmentsSent=2}",
    ) + &step("exceed", "set_monitored_value", &payload("value='95'"))
        + &step(
            "alert",
            "await_monitoring",
            "expect_response={eventsAcknowledged=1,eventsQueued=0,lastEventOutcome='Result'}",
        )
        + &step(
            "burst",
            "emit_monitoring_events",
            &payload("count=3,monitorId=4031"),
        )
        + &step(
            "burst-sent",
            "await_monitoring",
            "expect_response={eventsAcknowledged=3,eventsEmitted=4}",
        )
        + &step(
            "cleared",
            "await_monitoring",
            "expect_response={monitorIds=[4003,4030],monitorCount=2,hardWired=1}",
        )
        + &step("disconnect", "disconnect", "");
    let mut process = start(
        &directory,
        &listener,
        &json!({"monitors_per_fragment": 2, "events_per_message": 2}),
        &steps,
    );
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    let request = json!({"setMonitoringData": [item(&connector(), "Temperature", "UpperThreshold", 75.0, 3)]});
    assert_eq!(
        call(&mut socket, "set", "SetVariableMonitoring", request).await[2]["setMonitoringResult"]
            [0]["id"],
        4031
    );
    assert_eq!(
        call(
            &mut socket,
            "report",
            "GetMonitoringReport",
            json!({"requestId": 61})
        )
        .await[2],
        json!({"status": "Accepted"})
    );
    for number in 0..2 {
        let fragment = expect(&mut socket, "NotifyMonitoringReport").await;
        assert_eq!(
            (&fragment["requestId"], &fragment["seqNo"], &fragment["tbc"]),
            (&json!(61), &json!(number), &json!(number == 0))
        );
    }
    let alert = expect(&mut socket, "NotifyEvent").await;
    assert_eq!(
        (
            &alert["eventData"][0]["trigger"],
            &alert["eventData"][0]["actualValue"]
        ),
        (&json!("Alerting"), &json!("95"))
    );
    // The burst of three events travels as two NotifyEvent messages: seqNo 0 (tbc) and 1.
    let first = expect(&mut socket, "NotifyEvent").await;
    let second = expect(&mut socket, "NotifyEvent").await;
    assert_eq!(
        (
            &first["seqNo"],
            &first["tbc"],
            &second["seqNo"],
            &second["tbc"]
        ),
        (&json!(0), &json!(true), &json!(1), &json!(false))
    );
    let cleared = call(
        &mut socket,
        "clear",
        "ClearVariableMonitoring",
        json!({"id": [4031]}),
    )
    .await;
    assert_eq!(cleared[2]["clearMonitoringResult"][0]["status"], "Accepted");
    closed(&mut socket).await;
    let output = process.finish().await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[tokio::test]
async fn a_failed_assertion_fails_the_process_with_a_stable_code() {
    let directory = PrivateDirectory::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let steps = step(
        "expect-monitor",
        "assert_monitoring",
        "expect_response={monitorIds=[4003,4030,4031]}",
    );
    let mut process = start(&directory, &listener, &json!({}), &steps);
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    let output = process.finish().await;
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("monitoring_assertion_failed"), "{text}");
}

#[tokio::test]
async fn a_report_cut_short_by_a_closed_socket_is_visible_to_the_scenario() {
    let directory = PrivateDirectory::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let steps = step(
        "interrupted",
        "await_monitoring",
        "expect_response={reportsDelivered=0,lastReport={requestId=62,planned=4,sent=1,outcome='Interrupted'}}",
    );
    let mut process = start(
        &directory,
        &listener,
        &json!({"monitors_per_fragment": 1, "report_fault": "disconnect_mid_report"}),
        &steps,
    );
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    let request = json!({"setMonitoringData": [item(&evse(), "Power", "Delta", 2.5, 6), item(&station_component(), "SupplyVoltage", "LowerThreshold", 207.0, 4)]});
    call(&mut socket, "set", "SetVariableMonitoring", request).await;
    let reply = call(&mut socket, "report", "GetMonitoringReport", json!({"requestId": 62, "componentVariable": [{"component": {"name": "EVSE"}}, {"component": station_component()}]})).await;
    assert_eq!(reply[2]["status"], "Accepted");
    expect(&mut socket, "NotifyMonitoringReport").await;
    closed(&mut socket).await;
    let output = process.finish().await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
