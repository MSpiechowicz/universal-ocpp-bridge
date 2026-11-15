//! 2.0.1 monitoring station behavior over a real OCPP-J socket: setup and removal, report
//! fragments, event bursts, invalid monitors and the deliberate misbehaviour knobs.
mod monitoring201_support;
#[allow(dead_code)]
#[path = "local_authorization201/socket.rs"]
mod socket;
use monitoring201_support::{assert_valid, connector, document, evse, item, station_component};
use serde_json::{Value, json};
use socket::{accept, boot, receive, send};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use uob_sim::scenario::parse_configuration;
use uob_sim::{ProtocolClient, SimulatorAction, SimulatorCall, SimulatorProtocolClient};

type Socket = WebSocketStream<TcpStream>;

async fn station(listener: &TcpListener, fields: Value) -> SimulatorProtocolClient {
    let source = document(
        &format!("ws://{}/ocpp/alpha", listener.local_addr().unwrap()),
        fields,
    );
    let client = SimulatorProtocolClient::connect(
        parse_configuration(&source).unwrap().stations[0].client_config(),
    )
    .await
    .unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: json!({"reason":"PowerUp","chargingStation":{"model":"Monitor201","vendorName":"UOB"}}),
        })
        .await
        .unwrap();
    client
}

async fn call(socket: &mut Socket, id: &str, action: &str, payload: Value) -> Value {
    send(socket, json!([2, id, action, payload])).await;
    let frame = receive(socket).await;
    assert_eq!(frame[1], id, "{frame}");
    frame
}

/// Acknowledge the next station CALL, which must be this action; returns its payload.
async fn expect_call(socket: &mut Socket, action: &str) -> Value {
    let frame = receive(socket).await;
    assert_eq!(
        (frame[0].as_u64(), frame[2].as_str()),
        (Some(2), Some(action)),
        "{frame}"
    );
    assert_valid(&format!("{action}Request"), &frame[3]);
    send(socket, json!([3, frame[1], {}])).await;
    frame[3].clone()
}

async fn quiet(socket: &mut Socket) -> bool {
    tokio::time::timeout(Duration::from_millis(400), receive(socket))
        .await
        .is_err()
}

async fn run<T>(peer: impl Future<Output = ()>, client: impl Future<Output = T>) -> T {
    tokio::time::timeout(
        Duration::from_secs(20),
        Box::pin(async { tokio::join!(peer, client).1 }),
    )
    .await
    .unwrap()
}

fn three_monitors() -> Value {
    json!({"setMonitoringData": [
        item(&connector(), "Temperature", "UpperThreshold", 75.0, 3),
        item(&evse(), "Power", "Delta", 2.5, 6),
        item(&station_component(), "SupplyVoltage", "LowerThreshold", 207.0, 4)]})
}

#[tokio::test]
async fn monitors_are_set_listed_in_fragments_and_cleared() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let reply = call(
            &mut socket,
            "set",
            "SetVariableMonitoring",
            three_monitors(),
        )
        .await;
        assert_valid("SetVariableMonitoringResponse", &reply[2]);
        let ids: Vec<_> = reply[2]["setMonitoringResult"]
            .as_array()
            .unwrap()
            .iter()
            .map(|result| result["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, [4031, 4032, 4033]);
        // Five monitors in fragments of two: three NotifyMonitoringReport calls after the reply.
        let reply = call(
            &mut socket,
            "report",
            "GetMonitoringReport",
            json!({"requestId": 41}),
        )
        .await;
        assert_eq!(reply[2], json!({"status": "Accepted"}));
        let mut sequence = Vec::new();
        for _ in 0..3 {
            let fragment = expect_call(&mut socket, "NotifyMonitoringReport").await;
            assert_eq!(fragment["requestId"], 41);
            sequence.push((
                fragment["seqNo"].as_i64().unwrap(),
                fragment["tbc"].as_bool().unwrap(),
            ));
        }
        assert_eq!(sequence, [(0, true), (1, true), (2, false)]);
        let cleared = call(
            &mut socket,
            "clear",
            "ClearVariableMonitoring",
            json!({"id": [4031, 4003, 5000]}),
        )
        .await;
        assert_valid("ClearVariableMonitoringResponse", &cleared[2]);
        let statuses: Vec<_> = cleared[2]["clearMonitoringResult"]
            .as_array()
            .unwrap()
            .iter()
            .map(|result| result["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, ["Accepted", "Rejected", "NotFound"]);
        // The cleared monitor is gone from the next report: 4 monitors, two fragments.
        call(
            &mut socket,
            "again",
            "GetMonitoringReport",
            json!({"requestId": 42}),
        )
        .await;
        let mut listed = Vec::new();
        for _ in 0..2 {
            let fragment = expect_call(&mut socket, "NotifyMonitoringReport").await;
            for monitor in fragment["monitor"].as_array().unwrap() {
                for entry in monitor["variableMonitoring"].as_array().unwrap() {
                    listed.push(entry["id"].as_i64().unwrap());
                }
            }
        }
        listed.sort_unstable();
        assert_eq!(listed, [4003, 4030, 4032, 4033]);
        let empty = call(
            &mut socket,
            "none",
            "GetMonitoringReport",
            json!({"requestId": 43, "componentVariable": [{"component": connector()}]}),
        )
        .await;
        assert_eq!(empty[2]["status"], "EmptyResultSet");
        assert!(
            quiet(&mut socket).await,
            "an empty report sends no fragment"
        );
    };
    let client = station(&listener, json!({"monitors_per_fragment": 2}));
    let client = run(peer, client).await;
    let snapshot = client.monitoring201().unwrap().snapshot();
    assert_eq!(
        (
            &snapshot["reportsDelivered"],
            &snapshot["reportFragmentsSent"]
        ),
        (&json!(2), &json!(5))
    );
    client.abort();
}

#[tokio::test]
async fn invalid_requests_are_refused_and_unknown_monitors_report_their_status() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let invalid = json!({"setMonitoringData": [
            item(&connector(), "Temperature", "Delta", -1.0, 3),
            item(&json!({"name": "Heater"}), "Temperature", "Delta", 1.0, 3),
            item(&evse(), "Power", "UpperThreshold", 99999.0, 3)]});
        let reply = call(&mut socket, "invalid", "SetVariableMonitoring", invalid).await;
        assert_valid("SetVariableMonitoringResponse", &reply[2]);
        let statuses: Vec<_> = reply[2]["setMonitoringResult"]
            .as_array()
            .unwrap()
            .iter()
            .map(|result| result["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, ["Rejected", "UnknownComponent", "Rejected"]);
        for (id, action, payload, code) in [
            (
                "shape",
                "SetVariableMonitoring",
                json!({"setMonitoringData": [{"value": 1}]}),
                "FormationViolation",
            ),
            (
                "extra",
                "SetMonitoringLevel",
                json!({"severity": 1, "unexpected": true}),
                "FormationViolation",
            ),
            (
                "many",
                "ClearVariableMonitoring",
                json!({"id": [1, 2, 3]}),
                "OccurrenceConstraintViolation",
            ),
        ] {
            let reply = call(&mut socket, id, action, payload).await;
            assert_eq!(
                (reply[0].as_u64(), reply[2].as_str()),
                (Some(4), Some(code)),
                "{reply}"
            );
        }
        let level = call(
            &mut socket,
            "level",
            "SetMonitoringLevel",
            json!({"severity": 10}),
        )
        .await;
        assert_eq!(level[2]["status"], "Rejected");
        let base = call(
            &mut socket,
            "base",
            "SetMonitoringBase",
            json!({"monitoringBase": "HardWiredOnly"}),
        )
        .await;
        assert_eq!(base[2]["status"], "NotSupported");
    };
    let client = station(
        &listener,
        json!({"items_per_message_clear": 2, "supported_bases": ["All"]}),
    );
    let client = run(peer, client).await;
    client.abort();
}

#[tokio::test]
async fn a_station_without_a_monitoring_model_leaves_the_calls_unimplemented() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let reply = call(
            &mut socket,
            "level",
            "SetMonitoringLevel",
            json!({"severity": 1}),
        )
        .await;
        assert_eq!(
            (reply[0].as_u64(), reply[2].as_str()),
            (Some(4), Some("NotImplemented"))
        );
    };
    let source = format!(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}/ocpp/alpha'\nocpp_version='2.0.1'\nrequest_timeout_ms=1000\n[[stations.evses]]\nid=1\nconnectors=[1]\n",
        listener.local_addr().unwrap()
    );
    let client = async {
        let client = SimulatorProtocolClient::connect(
            parse_configuration(&source).unwrap().stations[0].client_config(),
        )
        .await
        .unwrap();
        client.call(SimulatorCall { action: SimulatorAction::BootNotification,
            payload: json!({"reason":"PowerUp","chargingStation":{"model":"Plain","vendorName":"UOB"}}) }).await.unwrap();
        client
    };
    let client = run(peer, client).await;
    assert!(client.monitoring201().is_none());
    client.abort();
}

#[path = "monitoring201_wire/device_model.rs"]
mod device_model;
#[path = "monitoring201_wire/events.rs"]
mod events;
#[path = "monitoring201_wire/faults.rs"]
mod faults;
