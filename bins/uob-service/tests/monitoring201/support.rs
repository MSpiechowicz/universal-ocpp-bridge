pub use crate::host::{
    CONTROL, Fixture, PRIVILEGED, Process, READ, Socket, begin, client, completed, no_call,
    receive, result, send, station_call,
};
use serde_json::{Value, json};
use std::{fs, time::Duration};

pub const ALL_OPTIONS: &str = "get_variables=true\nset_variable_monitoring=true\n\
    clear_variable_monitoring=true\nget_monitoring_report=true\nset_monitoring_base=true\n\
    set_monitoring_level=true";

/// Daemons are started one at a time: ports are picked before they are bound, and starting many
/// daemons at once makes a port collision likely enough to flake a whole run.
static STARTUP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A running daemon for a fresh fixture, once its listeners accept connections.
pub async fn launch(options: &str) -> (Fixture, Process) {
    let _startup = STARTUP.lock().await;
    let fixture = fixture(options);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    (fixture, process)
}

/// Starts the daemon again on the same state after the previous one was dropped.
pub async fn restart(fixture: &Fixture) -> Process {
    let _startup = STARTUP.lock().await;
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    process
}

/// Station A becomes an OCPP 2.0.1 station with exactly the given option lines; station B stays
/// an OCPP 1.6 station without any.
fn fixture(options: &str) -> Fixture {
    let fixture = Fixture::new();
    let path = fixture.root.join("bridge.toml");
    let config = fs::read_to_string(&path)
        .unwrap()
        .replacen("protocol='ocpp16j'", "protocol='ocpp201'", 1)
        .replacen("get_composite_schedule=true", options, 1)
        .replacen(
            "connector_id='one'\nnative_connector_id=1",
            "evse_id='one'\nnative_evse_id=1",
            1,
        )
        .replacen(
            "connector_id='two'\nnative_connector_id=2",
            "evse_id='two'\nnative_evse_id=2",
            1,
        );
    fs::write(path, config).unwrap();
    fixture
}

pub async fn connected(fixture: &Fixture, client: &reqwest::Client) -> Socket {
    let mut socket = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut socket).await;
    fixture.connected(client, "station-a").await;
    socket
}

pub async fn boot(socket: &mut Socket) {
    let reply = station_call(
        socket,
        "boot",
        "BootNotification",
        json!({"chargingStation":{"model":"model","vendorName":"vendor"},"reason":"PowerUp"}),
    )
    .await;
    assert_eq!(reply["status"], "Accepted");
}

pub fn command(id: &str, action: &str, payload: Value) -> Value {
    let mut body = json!({"request_id":id,
        "resource":{"bridge_id":"bridge-1","station_id":"station-a"},
        "operation":{"kind":"ocpp","parameters":{"protocol":"ocpp201","action":action,
            "payload_schema":format!("urn:OCPP:Cp:2:2020:3:{action}Request")}},
        "expires_at":"2099-01-01T00:00:00Z"});
    body["operation"]["parameters"]["payload"] = payload;
    body
}

/// Submits `body`, expects the native CALL, answers it and returns the call and the HTTP result.
pub async fn answered(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    body: Value,
    reply: Value,
) -> (Value, Value) {
    let action = body["operation"]["parameters"]["action"].clone();
    let submission = begin(client, fixture, body);
    let call = receive(socket).await;
    assert_eq!(call[0], 2);
    assert_eq!(call[2], action);
    send(socket, json!([3, call[1], reply])).await;
    (call, completed(submission).await)
}

/// A command the management API refuses; its status and body are returned.
pub async fn refused(client: &reqwest::Client, fixture: &Fixture, body: Value) -> (u16, Value) {
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

/// The station reports its `MonitoringCtrlr` message limits, which the bridge learns before it
/// sends more than one monitor or ID in a request.
pub async fn learn_limits(client: &reqwest::Client, fixture: &Fixture, socket: &mut Socket) {
    for (index, (instance, variable)) in [
        ("SetVariableMonitoring", "ItemsPerMessage"),
        ("SetVariableMonitoring", "BytesPerMessage"),
        ("ClearVariableMonitoring", "ItemsPerMessage"),
        ("ClearVariableMonitoring", "BytesPerMessage"),
    ]
    .into_iter()
    .enumerate()
    {
        let entry = json!({"component":{"name":"MonitoringCtrlr"},
            "variable":{"name":variable,"instance":instance}});
        let mut reply = entry.clone();
        reply["attributeStatus"] = json!("Accepted");
        reply["attributeValue"] = json!(if variable == "ItemsPerMessage" {
            "8"
        } else {
            "4096"
        });
        let body = command(
            &format!("limit-{index}"),
            "GetVariables",
            json!({"getVariableData":[entry]}),
        );
        answered(
            client,
            fixture,
            socket,
            body,
            json!({"getVariableResult":[reply]}),
        )
        .await;
    }
}

/// Polls the command result until its report is no longer pending.
pub async fn settled(client: &reqwest::Client, fixture: &Fixture, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let value = result(client, fixture, id).await;
            if value["monitoring_201"]["outcome"]["report"]["state"] != "pending" {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("report leaves the pending state")
}

pub fn report_request(id: i32) -> Value {
    json!({"requestId":id})
}

/// One native `NotifyMonitoringReport` CALL with the given monitors and sequence number.
pub fn fragment(call_id: &str, request_id: i32, seq_no: u32, tbc: bool, monitors: &[i32]) -> Value {
    let monitor: Vec<Value> = monitors
        .iter()
        .map(|id| {
            json!({"component":{"name":"EVSE","evse":{"id":1}},"variable":{"name":"Power"},
                "variableMonitoring":[{"id":id,"transaction":false,"value":2.5,"type":"Delta",
                    "severity":6}]})
        })
        .collect();
    json!([2, call_id, "NotifyMonitoringReport", {
        "requestId":request_id,"seqNo":seq_no,"tbc":tbc,
        "generatedAt":"2026-11-10T09:15:00Z","monitor":monitor}])
}

/// Sends a report fragment and expects the empty native response.
pub async fn deliver(socket: &mut Socket, frame: Value) {
    let id = frame[1].clone();
    send(socket, frame).await;
    let reply = receive(socket).await;
    assert_eq!(reply, json!([3, id, {}]));
}

/// One native `NotifyEvent` CALL with `count` events.
pub fn event_call(call_id: &str, seq_no: u32, tbc: bool, count: u32) -> Value {
    let events: Vec<Value> = (0..count)
        .map(|index| {
            json!({"eventId":9000 + index,"timestamp":"2026-11-10T10:00:00Z","trigger":"Alerting",
                "actualValue":"81.0","variableMonitoringId":4001,
                "eventNotificationType":"CustomMonitor",
                "component":{"name":"Connector","evse":{"id":1,"connectorId":1}},
                "variable":{"name":"Temperature"}})
        })
        .collect();
    json!([2, call_id, "NotifyEvent", {
        "generatedAt":"2026-11-10T10:00:00Z","seqNo":seq_no,"tbc":tbc,"eventData":events}])
}

/// Live server-sent journal events of the monitoring type for station A.
pub struct Journal {
    response: reqwest::Response,
    pending: String,
}

impl Journal {
    pub async fn open(client: &reqwest::Client, fixture: &Fixture) -> Self {
        let response = client
            .get(
                fixture
                    .url("/api/v1/events?station_id=station-a&types=station.monitoring_event.201"),
            )
            .bearer_auth(READ)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        Self {
            response,
            pending: String::new(),
        }
    }

    pub async fn next(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(end) = self.pending.find("\n\n") {
                    let frame: String = self.pending.drain(..end + 2).collect();
                    if let Some(data) = frame.lines().find_map(|line| line.strip_prefix("data: ")) {
                        return serde_json::from_str(data).expect("valid journal event");
                    }
                    continue;
                }
                let bytes = self.response.chunk().await.unwrap().expect("open stream");
                self.pending.push_str(std::str::from_utf8(&bytes).unwrap());
                assert!(self.pending.len() < 1024 * 1024, "bounded frame");
            }
        })
        .await
        .expect("journal event arrives")
    }

    /// Whether another monitoring event arrives within a short grace period.
    pub async fn quiet(&mut self) -> bool {
        tokio::time::timeout(Duration::from_millis(400), self.next())
            .await
            .is_err()
    }
}

/// The daemon's runtime counter of diagnostics dropped under resource pressure.
pub async fn dropped_diagnostics(client: &reqwest::Client, fixture: &Fixture) -> u64 {
    let body = client
        .get(fixture.url("/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    body.lines()
        .find_map(|line| line.strip_prefix("uob_dropped_diagnostics_total "))
        .expect("dropped diagnostics metric")
        .trim()
        .parse()
        .unwrap()
}

/// Sends `GetMonitoringReport`, answers it `Accepted` and returns the still-pending result.
pub async fn start_report(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    id: &str,
    request_id: i32,
) -> Value {
    let (call, pending) = answered(
        client,
        fixture,
        socket,
        command(id, "GetMonitoringReport", report_request(request_id)),
        json!({"status":"Accepted"}),
    )
    .await;
    assert_eq!(call[3], json!({"requestId":request_id}));
    assert_eq!(
        pending["monitoring_201"]["outcome"]["report"]["state"],
        "pending"
    );
    pending
}

/// Sends a frame and returns the station-visible reply, whatever it is.
pub async fn exchange(socket: &mut Socket, frame: Value) -> Value {
    send(socket, frame).await;
    receive(socket).await
}
