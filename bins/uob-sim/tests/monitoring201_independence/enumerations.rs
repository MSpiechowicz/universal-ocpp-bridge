//! Every emitted status and event field is a value of the pinned schema enumerations.
use crate::monitoring201_support::{call, config, drain_events, item, set, station};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn schema(name: &str) -> Value {
    let path = format!(
        "{}/../../tests/ocpp-fixtures/corpus/schemas/2.0.1/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn enumeration(name: &str, definition: &str) -> BTreeSet<String> {
    schema(name)["definitions"][definition]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn every_emitted_status_and_event_field_is_in_the_pinned_enumerations() {
    let station = station(json!({"monitors_per_fragment": 1}));
    let mut set_statuses = BTreeSet::new();
    let heater = json!({"name": "Heater"});
    let power = json!({"name": "EVSE", "evse": {"id": 1}});
    let voltage = json!({"name": "ChargingStation"});
    for request in [
        item(&heater, "Temperature", "Delta", 1.0, 3),
        item(&voltage, "Nothing", "Delta", 1.0, 3),
        item(&voltage, "Tamper", "Delta", 1.0, 3),
        item(&power, "Power", "UpperThreshold", 99999.0, 3),
        item(&power, "Power", "Delta", 1.0, 3),
        item(&power, "Power", "Delta", 2.0, 3),
    ] {
        for result in set(&station, json!([request])) {
            set_statuses.insert(result["status"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(
        set_statuses,
        enumeration(
            "SetVariableMonitoringResponse",
            "SetMonitoringStatusEnumType"
        )
    );
    let mut clear_statuses = BTreeSet::new();
    for id in [4999, 4003, 4030] {
        let reply = call(&station, "ClearVariableMonitoring", &json!({"id": [id]}));
        clear_statuses.insert(
            reply["clearMonitoringResult"][0]["status"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    assert_eq!(
        clear_statuses,
        enumeration(
            "ClearVariableMonitoringResponse",
            "ClearMonitoringStatusEnumType"
        )
    );
    let level: BTreeSet<_> = [3, 10]
        .iter()
        .map(|severity| {
            call(
                &station,
                "SetMonitoringLevel",
                &json!({"severity": severity}),
            )["status"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        level,
        enumeration("SetMonitoringLevelResponse", "GenericStatusEnumType")
    );
    let generic = enumeration(
        "GetMonitoringReportResponse",
        "GenericDeviceModelStatusEnumType",
    );
    let report: BTreeSet<_> = [
        json!({"requestId": 1}),
        json!({"requestId": 1, "componentVariable": [{"component": {"name": "Heater"}}]}),
    ]
    .iter()
    .map(|payload| {
        call(&station, "GetMonitoringReport", payload)["status"]
            .as_str()
            .unwrap()
            .to_owned()
    })
    .chain(["Accepted"].map(str::to_owned))
    .collect();
    assert!(
        report.is_subset(&generic) && report.len() == 2,
        "{report:?}"
    );
    let limited = self::station(json!({"supported_criteria": ["ThresholdMonitoring"]}));
    let unsupported = call(
        &limited,
        "GetMonitoringReport",
        &json!({"requestId": 1, "monitoringCriteria": ["DeltaMonitoring"]}),
    );
    assert!(generic.contains(unsupported["status"].as_str().unwrap()));
}

#[test]
fn every_emitted_event_field_is_in_the_pinned_enumerations() {
    let voltage = json!({"name": "ChargingStation"});
    let power = json!({"name": "EVSE", "evse": {"id": 1}});
    // Every trigger, notification type and monitor type is a pinned value.
    let triggers = enumeration("NotifyEventRequest", "EventTriggerEnumType");
    let notifications = enumeration("NotifyEventRequest", "EventNotificationEnumType");
    let events = self::station(json!({}));
    events
        .set_value(&voltage, &json!({"name": "SupplyVoltage"}), "290")
        .unwrap();
    events
        .set_value(&power, &json!({"name": "Power"}), "21000")
        .unwrap();
    set(
        &events,
        json!([
            item(&power, "Power", "Delta", 1.0, 3),
            item(
                &json!({"name": "TemperatureSensor", "instance": "Cabinet"}),
                "Temperature",
                "Periodic",
                60.0,
                9
            )
        ]),
    );
    events
        .set_value(&power, &json!({"name": "Power"}), "100")
        .unwrap();
    events.advance(time::OffsetDateTime::now_utc() + time::Duration::seconds(120));
    let mut seen_triggers = BTreeSet::new();
    let mut seen_notifications = BTreeSet::new();
    for message in drain_events(&events) {
        for data in message["eventData"].as_array().unwrap() {
            seen_triggers.insert(data["trigger"].as_str().unwrap().to_owned());
            seen_notifications.insert(data["eventNotificationType"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(seen_triggers, triggers);
    assert!(seen_notifications.is_subset(&notifications));
    let expected: BTreeSet<String> = ["HardWiredMonitor", "PreconfiguredMonitor", "CustomMonitor"]
        .map(str::to_owned)
        .into();
    assert_eq!(seen_notifications, expected);
    let types = enumeration("SetVariableMonitoringRequest", "MonitorEnumType");
    let configured: BTreeSet<_> = config(json!({}))
        .variables
        .iter()
        .flat_map(|variable| {
            variable
                .monitor_types
                .iter()
                .map(|kind| kind.as_str().to_owned())
        })
        .collect();
    assert!(configured.is_subset(&types));
}
