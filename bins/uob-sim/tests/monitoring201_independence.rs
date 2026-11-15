//! The 2.0.1 monitoring station model is derived from the pinned OCA sources and the independently
//! authored corpus, never from bridge code: no bridge crate is reachable, every emitted value is
//! checked against the pinned schemas and enumerations, and the corpus frames drive and judge it.
#[path = "monitoring201_independence/enumerations.rs"]
mod enumerations;
mod monitoring201_support;
use monitoring201_support::{
    assert_valid, call, corpus_frame, drain_events, is_valid, item, set, station,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn keys(value: &Value) -> BTreeSet<String> {
    value.as_object().unwrap().keys().cloned().collect()
}

/// Drop what a station chooses itself: the generated ids and the free-text additional info.
fn comparable(mut reply: Value) -> Value {
    for result in reply["setMonitoringResult"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        result.as_object_mut().unwrap().remove("id");
        if let Some(info) = result.get_mut("statusInfo") {
            info.as_object_mut().unwrap().remove("additionalInfo");
        }
    }
    reply
}

#[test]
fn simulator_reaches_no_bridge_crate() {
    let manifest = include_str!("../Cargo.toml");
    let bridge: Vec<_> = manifest
        .lines()
        .filter(|line| line.starts_with("uob"))
        .collect();
    assert!(
        bridge.is_empty(),
        "simulator depends on a bridge crate: {bridge:?}"
    );
    for source in [
        include_str!("../src/monitoring201.rs"),
        include_str!("../src/monitoring201/config.rs"),
        include_str!("../src/monitoring201/device.rs"),
        include_str!("../src/monitoring201/events.rs"),
        include_str!("../src/monitoring201/model.rs"),
        include_str!("../src/monitoring201/ops.rs"),
        include_str!("../src/monitoring201/report.rs"),
        include_str!("../src/monitoring201/request.rs"),
        include_str!("../src/monitoring201/transport.rs"),
        include_str!("../src/scenario/monitoring201.rs"),
        include_str!("../src/local_authorization201/transport/monitoring.rs"),
    ] {
        assert!(!source.contains("uob_"), "bridge path in simulator source");
    }
}

#[test]
fn corpus_set_requests_drive_the_station_to_the_corpus_replies() {
    for (request, reply) in [
        ("monitoring-set-all-types", "monitoring-set-reply-accepted"),
        ("monitoring-set-minimal", "monitoring-set-reply-minimal"),
        (
            "monitoring-set-delta-non-numeric",
            "monitoring-set-reply-delta-non-numeric",
        ),
        ("monitoring-set-mixed", "monitoring-set-reply-mixed"),
        (
            "monitoring-set-replace-id",
            "monitoring-set-reply-rejected-unknown-id",
        ),
    ] {
        let station = station(json!({}));
        let actual = call(&station, "SetVariableMonitoring", &corpus_frame(request)[3]);
        let expected = corpus_frame(reply)[2].clone();
        assert_eq!(comparable(actual), comparable(expected), "{request}");
    }
}

#[test]
fn corpus_clear_base_and_level_requests_get_the_corpus_replies() {
    let station = station(json!({}));
    let added = set(
        &station,
        json!([item(
            &json!({"name": "Connector", "evse": {"id": 1, "connectorId": 1}}),
            "Temperature",
            "UpperThreshold",
            75.0,
            3
        )]),
    );
    let id = added[0]["id"].clone();
    let actual = call(&station, "ClearVariableMonitoring", &json!({"id": [id]}));
    assert_eq!(
        actual["clearMonitoringResult"][0]["status"],
        corpus_frame("monitoring-clear-reply-accepted")[2]["clearMonitoringResult"][0]["status"]
    );
    let actual = call(
        &station,
        "ClearVariableMonitoring",
        &corpus_frame("monitoring-clear-single")[3],
    );
    assert_eq!(
        actual["clearMonitoringResult"][0]["status"],
        corpus_frame("monitoring-clear-reply-not-found")[2]["clearMonitoringResult"][0]["status"]
    );
    // The corpus's multi-clear: one cleared, one unknown, one hard-wired (id 4003 here).
    let actual = call(
        &station,
        "ClearVariableMonitoring",
        &json!({"id": [4030, 4002, 4003]}),
    );
    let statuses: Vec<_> = actual["clearMonitoringResult"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["status"].clone())
        .collect();
    let corpus =
        corpus_frame("monitoring-clear-reply-multi-mixed")[2]["clearMonitoringResult"].clone();
    let expected: Vec<_> = corpus
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["status"].clone())
        .collect();
    assert_eq!(statuses, expected);
    assert_eq!(
        actual["clearMonitoringResult"][2]["statusInfo"],
        json!({"reasonCode": "HardWired"})
    );
    for name in [
        "monitoring-base-all",
        "monitoring-base-factory-default",
        "monitoring-base-hard-wired-only",
    ] {
        let actual = call(&station, "SetMonitoringBase", &corpus_frame(name)[3]);
        assert_eq!(
            actual,
            corpus_frame("monitoring-base-reply-accepted")[2],
            "{name}"
        );
    }
    let limited = self::station(json!({"supported_bases": ["All"]}));
    let actual = call(
        &limited,
        "SetMonitoringBase",
        &corpus_frame("monitoring-base-factory-default")[3],
    );
    assert_eq!(
        actual,
        corpus_frame("monitoring-base-reply-not-supported")[2]
    );
    for name in [
        "monitoring-level-0",
        "monitoring-level-4",
        "monitoring-level-9",
    ] {
        let actual = call(&station, "SetMonitoringLevel", &corpus_frame(name)[3]);
        assert_eq!(
            actual,
            corpus_frame("monitoring-level-reply-accepted")[2],
            "{name}"
        );
    }
}

#[test]
fn corpus_report_requests_select_what_they_describe() {
    let station = station(json!({"monitors_per_fragment": 50}));
    set(
        &station,
        json!([
            item(
                &json!({"name": "Connector", "evse": {"id": 1, "connectorId": 1}}),
                "Temperature",
                "UpperThreshold",
                75.0,
                3
            ),
            item(
                &json!({"name": "EVSE", "evse": {"id": 1}}),
                "Power",
                "Delta",
                2.5,
                6
            ),
            item(
                &json!({"name": "TemperatureSensor", "instance": "Cabinet"}),
                "Temperature",
                "Periodic",
                300.0,
                9
            ),
        ]),
    );
    let listed = |name: &str| -> usize {
        let reply = station.handle_call("GetMonitoringReport", &corpus_frame(name)[3]);
        assert_valid("GetMonitoringReportResponse", &reply.payload);
        reply.report.map_or(0, |plan| {
            plan.frames
                .iter()
                .flat_map(|frame| frame["monitor"].as_array().unwrap())
                .map(|monitor| monitor["variableMonitoring"].as_array().unwrap().len())
                .sum()
        })
    };
    assert_eq!(listed("monitoring-get-report-no-filter"), 5);
    assert_eq!(listed("monitoring-get-report-criteria"), 3);
    assert_eq!(listed("monitoring-get-report-criteria-all"), 5);
    assert_eq!(listed("monitoring-get-report-both"), 1);
    assert_eq!(listed("monitoring-get-report-request-id-zero"), 5);
    // The corpus fragments have the keys the station's fragments have.
    let reply = station.handle_call(
        "GetMonitoringReport",
        &corpus_frame("monitoring-get-report-no-filter")[3],
    );
    let fragment = &reply.report.unwrap().frames[0];
    assert!(keys(fragment).is_subset(&keys(&corpus_frame("monitoring-report-fragment-0")[3])));
    assert_eq!(
        keys(&fragment["monitor"][0]),
        keys(&corpus_frame("monitoring-report-single")[3]["monitor"][0])
    );
}

#[test]
fn corpus_negative_requests_are_refused_or_rejected_per_item() {
    let cases = corpus_frame("monitoring-negative-cases");
    let (mut schema_refused, mut native_refused) = (0, 0);
    for case in cases["cases"].as_array().unwrap() {
        let wire = &case["wire"];
        let action = wire[2].as_str().unwrap_or_default();
        if wire[0] != 2
            || ![
                "SetVariableMonitoring",
                "ClearVariableMonitoring",
                "GetMonitoringReport",
                "SetMonitoringBase",
                "SetMonitoringLevel",
            ]
            .contains(&action)
        {
            continue;
        }
        let station = station(json!({}));
        let reply = station.handle_call(action, &wire[3]).payload;
        let id = &case["id"];
        match case["rule"].as_str().unwrap() {
            "schema" | "int32" => {
                assert_eq!(reply["callError"], "FormationViolation", "{id}");
                schema_refused += 1;
            }
            // Schema-valid but outside the specified range: refused per item, never applied.
            "severity-range" | "delta-negative" => {
                let status = reply["status"]
                    .as_str()
                    .or_else(|| reply["setMonitoringResult"][0]["status"].as_str());
                assert_eq!(status, Some("Rejected"), "{id}: {reply}");
                native_refused += 1;
            }
            other => panic!("unhandled request rule {other}"),
        }
        assert_eq!(
            station.snapshot()["monitorCount"],
            2,
            "{id} changed the monitors"
        );
    }
    assert!(
        schema_refused >= 25 && native_refused == 5,
        "{schema_refused} {native_refused}"
    );
}

#[test]
fn the_schema_interpreter_agrees_with_every_corpus_negative_case() {
    let cases = corpus_frame("monitoring-negative-cases");
    let mut checked = 0;
    for case in cases["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let wire = &case["wire"];
        let name = if wire[0] == 2 {
            wire[2].as_str().unwrap().to_owned() + "Request"
        } else if id.starts_with("set-reply") {
            "SetVariableMonitoringResponse".to_owned()
        } else if id.starts_with("clear-reply") {
            "ClearVariableMonitoringResponse".to_owned()
        } else if id.starts_with("base-reply") {
            "SetMonitoringBaseResponse".to_owned()
        } else if id.starts_with("level-reply") {
            "SetMonitoringLevelResponse".to_owned()
        } else if id.starts_with("get-report-reply") {
            "GetMonitoringReportResponse".to_owned()
        } else if id.starts_with("report-response") {
            "NotifyMonitoringReportResponse".to_owned()
        } else if id.starts_with("event-response") {
            "NotifyEventResponse".to_owned()
        } else {
            panic!("unmapped case {id}")
        };
        let payload = if wire[0] == 2 { &wire[3] } else { &wire[2] };
        assert_eq!(
            is_valid(&name, payload),
            case["schema_valid"].as_bool().unwrap(),
            "{id}"
        );
        checked += 1;
    }
    assert!(checked > 100, "{checked}");
}

#[test]
fn corpus_event_frames_have_the_fields_the_station_sends() {
    let station = station(json!({}));
    let component = json!({"name": "Connector", "evse": {"id": 1, "connectorId": 1}});
    set(
        &station,
        json!([item(&component, "Temperature", "UpperThreshold", 75.0, 3)]),
    );
    for value in ["80", "70"] {
        station
            .set_value(&component, &json!({"name": "Temperature"}), value)
            .unwrap();
    }
    let sent = drain_events(&station);
    for (message, corpus) in sent
        .iter()
        .zip(["monitoring-event-alerting", "monitoring-event-cleared"])
    {
        let expected = corpus_frame(corpus)[3].clone();
        assert_eq!(
            keys(message),
            keys(&expected)
                .into_iter()
                .chain(["tbc".to_owned()])
                .collect::<BTreeSet<_>>(),
            "{corpus}"
        );
        assert_eq!(
            keys(&message["eventData"][0]),
            keys(&expected["eventData"][0]),
            "{corpus}"
        );
    }
}
