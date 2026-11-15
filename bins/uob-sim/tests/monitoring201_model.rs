//! The 2.0.1 monitoring station model: monitor setup and removal, bases, levels and limits.
#[path = "monitoring201_model/events.rs"]
mod events;
mod monitoring201_support;
#[path = "monitoring201_model/report.rs"]
mod report;
use monitoring201_support::{
    cabinet, call, connector, evse, ids, item, set, station, station_component,
};
use serde_json::{Value, json};

fn statuses(results: &[Value]) -> Vec<&str> {
    results
        .iter()
        .map(|result| result["status"].as_str().unwrap())
        .collect()
}

fn kinds(station: &uob_sim::monitoring201::Monitoring201Handle) -> Vec<String> {
    station.snapshot()["monitors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|monitor| format!("{}:{}", monitor["id"], monitor["kind"].as_str().unwrap()))
        .collect()
}

#[test]
fn every_item_gets_its_own_result_with_the_requested_identity() {
    let station = station(json!({}));
    let heater = json!({"name": "Heater"});
    let items = json!([
        item(&connector(), "Temperature", "UpperThreshold", 75.0, 3),
        item(&heater, "Temperature", "UpperThreshold", 1.0, 5),
        item(
            &station_component(),
            "NoSuchVariable",
            "LowerThreshold",
            10.0,
            4
        ),
        item(
            &json!({"name": "Connector", "evse": {"id": 2, "connectorId": 1}}),
            "AvailabilityState",
            "Periodic",
            60.0,
            7
        ),
        item(&evse(), "Power", "UpperThreshold", 99999.0, 2),
        item(&connector(), "Temperature", "UpperThreshold", 70.0, 3),
        item(&station_component(), "Tamper", "Delta", 1.0, 3),
    ]);
    let results = set(&station, items.clone());
    // N04.FR.03-06 and N04.FR.10: one status per item, in request order.
    assert_eq!(
        statuses(&results),
        [
            "Accepted",
            "UnknownComponent",
            "UnknownVariable",
            "UnsupportedMonitorType",
            "Rejected",
            "Duplicate",
            "UnsupportedMonitorType"
        ]
    );
    assert_eq!(results[0]["id"], 4031);
    assert!(results[1..].iter().all(|result| result.get("id").is_none()));
    assert_eq!(results[4]["statusInfo"]["reasonCode"], "ValueOutOfRange");
    for (result, request) in results.iter().zip(items.as_array().unwrap()) {
        // N04.FR.02: the same component and variable as the item, and its type and severity.
        for field in ["component", "variable", "type", "severity"] {
            assert_eq!(result[field], request[field]);
        }
    }
    assert_eq!(ids(&station), [4003, 4030, 4031]);
}

#[test]
fn invalid_monitors_are_rejected_and_change_nothing() {
    let station = station(json!({}));
    let mut unknown_id = item(&connector(), "Temperature", "UpperThreshold", 75.0, 3);
    unknown_id["id"] = json!(999);
    let mut other_variable = item(
        &station_component(),
        "SupplyVoltage",
        "UpperThreshold",
        100.0,
        3,
    );
    other_variable["id"] = json!(4030);
    let mut hard_wired = item(
        &station_component(),
        "SupplyVoltage",
        "UpperThreshold",
        270.0,
        1,
    );
    hard_wired["id"] = json!(4003);
    let mut moved = item(&connector(), "Temperature", "UpperThreshold", 75.0, 3);
    moved["id"] = json!(4030);
    let cases = [
        (unknown_id, "UnknownId"),
        (other_variable, "ComponentMismatch"),
        (hard_wired, "HardWiredMonitor"),
        (moved, "ComponentMismatch"),
        (
            item(&connector(), "Temperature", "Delta", -1.0, 3),
            "InvalidDelta",
        ),
        (
            item(&connector(), "Temperature", "Delta", 0.0, 3),
            "InvalidDelta",
        ),
        (
            item(&connector(), "Temperature", "UpperThreshold", 150.5, 3),
            "ValueOutOfRange",
        ),
        (
            item(&connector(), "Temperature", "LowerThreshold", -41.0, 3),
            "ValueOutOfRange",
        ),
        (
            item(&cabinet(), "Temperature", "Periodic", 0.0, 3),
            "InvalidInterval",
        ),
        (
            item(&connector(), "Temperature", "UpperThreshold", 75.0, 10),
            "InvalidSeverity",
        ),
        (
            item(&connector(), "Temperature", "UpperThreshold", 75.0, -1),
            "InvalidSeverity",
        ),
    ];
    for (request, reason) in cases {
        let results = set(&station, json!([request]));
        assert_eq!(results[0]["status"], "Rejected", "{reason}");
        assert_eq!(results[0]["statusInfo"]["reasonCode"], reason);
        assert!(results[0].get("id").is_none());
    }
    assert_eq!(ids(&station), [4003, 4030]);
    // Boundary values are valid: a threshold may sit on the limit (N04.FR.06 note).
    let edge = set(
        &station,
        json!([item(
            &connector(),
            "Temperature",
            "UpperThreshold",
            150.0,
            3
        )]),
    );
    assert_eq!(edge[0]["status"], "Accepted");
}

#[test]
fn replacing_by_id_keeps_the_id_and_makes_a_preconfigured_monitor_custom() {
    let station = station(json!({}));
    assert_eq!(kinds(&station), ["4003:HardWired", "4030:Preconfigured"]);
    let mut replacement = item(&evse(), "Power", "UpperThreshold", 18000.0, 4);
    replacement["id"] = json!(4030);
    let results = set(&station, json!([replacement]));
    // N04.FR.12 and N04.FR.15.
    assert_eq!(
        (results[0]["status"].as_str(), &results[0]["id"]),
        (Some("Accepted"), &json!(4030))
    );
    let monitors = station.snapshot()["monitors"].clone();
    assert_eq!(monitors[1]["kind"], "Custom");
    assert_eq!(monitors[1]["value"], 18000.0);
    assert_eq!(ids(&station), [4003, 4030]);
    // The same type and severity on the same variable under another id is a duplicate.
    let duplicate = set(
        &station,
        json!([item(&evse(), "Power", "UpperThreshold", 17000.0, 4)]),
    );
    assert_eq!(duplicate[0]["status"], "Duplicate");
    // A different severity is a different monitor.
    let other = set(
        &station,
        json!([item(&evse(), "Power", "UpperThreshold", 17000.0, 5)]),
    );
    assert_eq!(other[0]["status"], "Accepted");
}

#[test]
fn clear_answers_per_id_and_refuses_hard_wired_monitors() {
    let station = station(json!({}));
    let added = set(
        &station,
        json!([item(&connector(), "Temperature", "UpperThreshold", 75.0, 3)]),
    );
    let id = added[0]["id"].as_i64().unwrap();
    let reply = call(
        &station,
        "ClearVariableMonitoring",
        &json!({"id": [id, 999, 4003, 4030]}),
    );
    let results = reply["clearMonitoringResult"].as_array().unwrap();
    // N06.FR.01-03 and N06.FR.05.
    assert_eq!(
        statuses(results),
        ["Accepted", "NotFound", "Rejected", "Accepted"]
    );
    assert_eq!(
        results
            .iter()
            .map(|result| result["id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [id, 999, 4003, 4030]
    );
    assert_eq!(results[2]["statusInfo"]["reasonCode"], "HardWired");
    assert_eq!(ids(&station), [4003]);
}

#[test]
fn monitoring_bases_keep_and_remove_the_right_monitors() {
    let station = station(json!({}));
    let base = |name: &str| {
        call(
            &station,
            "SetMonitoringBase",
            &json!({"monitoringBase": name}),
        )
    };
    set(
        &station,
        json!([item(&connector(), "Temperature", "UpperThreshold", 75.0, 3)]),
    );
    // N03.FR.05: only hard-wired monitors remain.
    assert_eq!(base("HardWiredOnly")["status"], "Accepted");
    assert_eq!(ids(&station), [4003]);
    // N03.FR.03: All activates the preconfigured monitors again.
    assert_eq!(base("All")["status"], "Accepted");
    assert_eq!(ids(&station), [4003, 4030]);
    let custom = set(
        &station,
        json!([item(&connector(), "Temperature", "UpperThreshold", 75.0, 3)]),
    );
    let mut replacement = item(&evse(), "Power", "UpperThreshold", 18000.0, 4);
    replacement["id"] = json!(4030);
    set(&station, json!([replacement]));
    // All leaves custom monitors, including a changed preconfigured one, intact.
    base("All");
    assert_eq!(
        ids(&station),
        [4003, 4030, custom[0]["id"].as_i64().unwrap()]
    );
    assert_eq!(station.snapshot()["monitors"][1]["kind"], "Custom");
    // N03.FR.04: FactoryDefault deletes every custom monitor and restores the preset.
    assert_eq!(base("FactoryDefault")["status"], "Accepted");
    assert_eq!(ids(&station), [4003, 4030]);
    assert_eq!(station.snapshot()["monitors"][1]["value"], 20000.0);
    assert_eq!(station.snapshot()["base"], "FactoryDefault");
}

#[test]
fn an_unsupported_base_is_not_supported_and_changes_nothing() {
    let station = station(json!({"supported_bases": ["All"]}));
    set(
        &station,
        json!([item(&connector(), "Temperature", "UpperThreshold", 75.0, 3)]),
    );
    let reply = call(
        &station,
        "SetMonitoringBase",
        &json!({"monitoringBase": "HardWiredOnly"}),
    );
    assert_eq!(reply["status"], "NotSupported");
    assert_eq!(ids(&station), [4003, 4030, 4031]);
}

#[test]
fn monitoring_level_accepts_zero_to_nine_only() {
    let station = station(json!({}));
    for level in 0..=9 {
        let reply = call(&station, "SetMonitoringLevel", &json!({"severity": level}));
        assert_eq!(reply["status"], "Accepted");
        assert_eq!(station.snapshot()["level"], level);
    }
    for level in [10, -1] {
        let reply = call(&station, "SetMonitoringLevel", &json!({"severity": level}));
        assert_eq!(reply["status"], "Rejected");
        assert_eq!(
            station.snapshot()["level"],
            9,
            "N05.FR.02 keeps the last level"
        );
    }
}

#[test]
fn message_limits_and_malformed_requests_are_call_errors() {
    let station = station(
        json!({"items_per_message_set": 2, "items_per_message_clear": 2,
        "bytes_per_message_set": 300, "bytes_per_message_clear": 20}),
    );
    let monitor = |severity| {
        item(
            &connector(),
            "Temperature",
            "UpperThreshold",
            75.0,
            severity,
        )
    };
    let code = |action: &str, payload: Value| call(&station, action, &payload)["callError"].clone();
    // N04.FR.09 and N06.FR.06/07.
    assert_eq!(
        code(
            "SetVariableMonitoring",
            json!({"setMonitoringData": [monitor(1), monitor(2), monitor(3)]})
        ),
        "OccurrenceConstraintViolation"
    );
    assert_eq!(
        code(
            "SetVariableMonitoring",
            json!({"setMonitoringData": [monitor(1), monitor(2)]})
        ),
        "FormatViolation"
    );
    assert_eq!(
        code("ClearVariableMonitoring", json!({"id": [1, 2, 3]})),
        "OccurrenceConstraintViolation"
    );
    assert_eq!(
        code("ClearVariableMonitoring", json!({"id": [100_000, 200_000]})),
        "FormatViolation"
    );
    for (action, payload) in [
        ("SetVariableMonitoring", json!({"setMonitoringData": []})),
        (
            "SetVariableMonitoring",
            json!({"setMonitoringData": [{"value": 1}]}),
        ),
        ("ClearVariableMonitoring", json!({"id": []})),
        ("SetMonitoringBase", json!({"monitoringBase": "Everything"})),
        ("SetMonitoringLevel", json!({"severity": 2.5})),
        ("GetMonitoringReport", json!({})),
        (
            "GetMonitoringReport",
            json!({"requestId": 1, "monitoringCriteria": []}),
        ),
    ] {
        assert_eq!(
            code(action, payload.clone()),
            "FormationViolation",
            "{action} {payload}"
        );
    }
    assert_eq!(
        ids(&station),
        [4003, 4030],
        "refused requests change nothing"
    );
}

#[test]
fn the_station_without_capacity_rejects_new_monitors() {
    let station = station(json!({"max_monitors": 3}));
    let first = set(
        &station,
        json!([item(&connector(), "Temperature", "UpperThreshold", 75.0, 3)]),
    );
    assert_eq!(first[0]["status"], "Accepted");
    let full = set(
        &station,
        json!([item(&connector(), "Temperature", "LowerThreshold", 0.0, 3)]),
    );
    assert_eq!(full[0]["statusInfo"]["reasonCode"], "NoCapacity");
    // A replacement needs no new slot.
    let mut replace = item(&connector(), "Temperature", "UpperThreshold", 76.0, 3);
    replace["id"] = first[0]["id"].clone();
    assert_eq!(set(&station, json!([replace]))[0]["status"], "Accepted");
}
