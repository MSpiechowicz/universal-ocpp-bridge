//! Shared drivers for the 2.0.1 monitoring station tests: seeded models, the pinned OCA schemas
//! and corpus wire frames.
#![allow(dead_code)]
mod schema;

#[allow(unused_imports)]
pub use schema::{assert_valid, corpus_frame, is_valid};
use serde_json::{Value, json};
use uob_sim::monitoring201::{Monitoring201Config, Monitoring201Handle};

/// The seeded device model: monitorable variables named like the independent corpus frames, one
/// hard-wired monitor (4003) and one preconfigured monitor (4030).
#[must_use]
pub fn seed() -> Value {
    json!({
        "variables": [
            {"component": {"name": "Connector", "evse": {"id": 1, "connectorId": 1}},
             "variable": {"name": "Temperature"}, "value": "40", "min": -40.0, "max": 150.0,
             "monitor_types": ["UpperThreshold", "LowerThreshold", "Delta"]},
            {"component": {"name": "ChargingStation"}, "variable": {"name": "SupplyVoltage"},
             "value": "230", "min": 0.0, "max": 300.0,
             "monitor_types": ["UpperThreshold", "LowerThreshold", "Delta"]},
            {"component": {"name": "EVSE", "evse": {"id": 1}}, "variable": {"name": "Power"},
             "value": "0", "min": 0.0, "max": 22000.0,
             "monitor_types": ["UpperThreshold", "LowerThreshold", "Delta"]},
            {"component": {"name": "TemperatureSensor", "instance": "Cabinet"},
             "variable": {"name": "Temperature"}, "value": "30", "min": -40.0, "max": 120.0,
             "monitor_types": ["UpperThreshold", "Delta", "Periodic", "PeriodicClockAligned"]},
            {"component": {"name": "Connector", "evse": {"id": 2, "connectorId": 1}},
             "variable": {"name": "AvailabilityState"}, "data_type": "string",
             "value": "Available", "monitor_types": ["Delta"]},
            {"component": {"name": "ChargingStation"}, "variable": {"name": "Tamper"},
             "data_type": "boolean", "value": "false", "monitor_types": []}
        ],
        "monitors": [
            {"id": 4003, "kind": "hard_wired", "component": {"name": "ChargingStation"},
             "variable": {"name": "SupplyVoltage"}, "type": "UpperThreshold", "value": 280.0,
             "severity": 1},
            {"id": 4030, "kind": "preconfigured", "component": {"name": "EVSE", "evse": {"id": 1}},
             "variable": {"name": "Power"}, "type": "UpperThreshold", "value": 20000.0,
             "severity": 4}
        ]
    })
}

/// The seeded model with `fields` overriding top-level configuration.
#[must_use]
pub fn config(fields: Value) -> Monitoring201Config {
    let mut base = seed();
    let Value::Object(fields) = fields else {
        panic!("configuration fields")
    };
    base.as_object_mut().unwrap().extend(fields);
    serde_json::from_value(base).unwrap()
}

#[must_use]
pub fn station(fields: Value) -> Monitoring201Handle {
    Monitoring201Handle::open(&config(fields)).unwrap()
}

/// Answer one CALL and check the reply against its pinned response schema.
pub fn call(station: &Monitoring201Handle, action: &str, payload: &Value) -> Value {
    let reply = station.handle_call(action, payload).payload;
    if reply.get("callError").is_none() {
        assert_valid(&format!("{action}Response"), &reply);
    }
    reply
}

pub fn set(station: &Monitoring201Handle, items: impl Into<Value>) -> Vec<Value> {
    let reply = call(
        station,
        "SetVariableMonitoring",
        &json!({"setMonitoringData": items.into()}),
    );
    reply["setMonitoringResult"]
        .as_array()
        .unwrap_or_else(|| panic!("{reply}"))
        .clone()
}

#[must_use]
pub fn item(component: &Value, variable: &str, kind: &str, value: f64, severity: i64) -> Value {
    json!({"component": component, "variable": {"name": variable}, "type": kind,
        "value": value, "severity": severity})
}

#[must_use]
pub fn connector() -> Value {
    json!({"name": "Connector", "evse": {"id": 1, "connectorId": 1}})
}

#[must_use]
pub fn evse() -> Value {
    json!({"name": "EVSE", "evse": {"id": 1}})
}

#[must_use]
pub fn station_component() -> Value {
    json!({"name": "ChargingStation"})
}

#[must_use]
pub fn cabinet() -> Value {
    json!({"name": "TemperatureSensor", "instance": "Cabinet"})
}

/// Monitor ids in force, from the station's own snapshot.
#[must_use]
pub fn ids(station: &Monitoring201Handle) -> Vec<i64> {
    station.snapshot()["monitorIds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_i64().unwrap())
        .collect()
}

/// Queued `NotifyEvent` messages, drained as acknowledged CALLs.
pub fn drain_events(station: &Monitoring201Handle) -> Vec<Value> {
    let now = time::OffsetDateTime::now_utc();
    let mut sent = Vec::new();
    while let Some(message) = station.next_event(now) {
        assert_valid("NotifyEventRequest", &message.to_request());
        station.event_finished(
            &message,
            uob_sim::monitoring201::EventOutcome201::Acknowledged,
            now,
        );
        sent.push(message.to_request());
    }
    sent
}

/// A one-station configuration document whose monitoring model is the seeded one plus `fields`.
#[must_use]
pub fn document(endpoint: &str, fields: Value) -> String {
    let mut monitoring = seed();
    let Value::Object(fields) = fields else {
        panic!("configuration fields")
    };
    monitoring.as_object_mut().unwrap().extend(fields);
    let document = json!({
        "schema_version": 1,
        "stations": [{
            "id": "alpha", "endpoint": endpoint, "ocpp_version": "2.0.1",
            "request_timeout_ms": 2000, "reconnect": false,
            "evses": [{"id": 1, "connectors": [1]}],
            "monitoring201": monitoring,
        }],
    });
    toml::to_string(&table_value(&document)).unwrap()
}

/// Converts explicitly: a workspace build enables `serde_json`'s `arbitrary_precision`, under
/// which serializing a JSON number straight into TOML does not produce a TOML number.
fn table_value(value: &Value) -> toml::Value {
    match value {
        Value::Null => panic!("TOML has no null"),
        Value::Bool(flag) => toml::Value::Boolean(*flag),
        Value::String(text) => toml::Value::String(text.clone()),
        Value::Array(items) => toml::Value::Array(items.iter().map(table_value).collect()),
        Value::Object(fields) => toml::Value::Table(
            fields
                .iter()
                .map(|(key, field)| (key.clone(), table_value(field)))
                .collect(),
        ),
        Value::Number(number) => match number.as_i64() {
            Some(integer) => toml::Value::Integer(integer),
            None => toml::Value::Float(number.as_f64().expect("finite number")),
        },
    }
}
