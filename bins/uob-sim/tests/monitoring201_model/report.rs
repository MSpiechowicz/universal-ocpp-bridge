//! `GetMonitoringReport`: selection, fragments and the deliberate misbehaviour knobs.
use crate::monitoring201_support::{
    assert_valid, cabinet, call, connector, evse, item, set, station, station_component,
};
use serde_json::{Value, json};
use uob_sim::monitoring201::{Monitoring201Handle, ReportPlan201};

fn request(station: &Monitoring201Handle, payload: &Value) -> (Value, Option<ReportPlan201>) {
    let reply = station.handle_call("GetMonitoringReport", payload);
    assert_valid("GetMonitoringReportResponse", &reply.payload);
    (reply.payload, reply.report)
}

/// Seven monitors on top of the two seeded ones.
fn populated(fields: Value) -> Monitoring201Handle {
    let station = station(fields);
    let results = set(
        &station,
        json!([
            item(&connector(), "Temperature", "UpperThreshold", 75.0, 3),
            item(&connector(), "Temperature", "LowerThreshold", -10.0, 4),
            item(&evse(), "Power", "Delta", 2.5, 6),
            item(&cabinet(), "Temperature", "Periodic", 300.0, 9),
            item(&cabinet(), "Temperature", "PeriodicClockAligned", 900.0, 8),
            item(
                &station_component(),
                "SupplyVoltage",
                "LowerThreshold",
                207.0,
                4
            ),
            item(&station_component(), "SupplyVoltage", "Delta", 10.0, 6),
        ]),
    );
    assert!(results.iter().all(|result| result["status"] == "Accepted"));
    station
}

/// Monitor ids in the report, in id order (grouping reorders monitors inside a fragment).
fn listed(plan: &ReportPlan201) -> Vec<i64> {
    let mut ids: Vec<i64> = plan
        .frames
        .iter()
        .flat_map(|frame| frame["monitor"].as_array().unwrap())
        .flat_map(|monitor| monitor["variableMonitoring"].as_array().unwrap())
        .map(|monitor| monitor["id"].as_i64().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn a_report_is_cut_into_numbered_fragments_with_the_same_request_id() {
    let station = populated(json!({"monitors_per_fragment": 4}));
    let (reply, plan) = request(&station, &json!({"requestId": 7001}));
    assert_eq!(reply, json!({"status": "Accepted"}));
    let plan = plan.unwrap();
    // 9 monitors in fragments of 4: N02.FR.03/04/09.
    let shape: Vec<_> = plan
        .frames
        .iter()
        .map(|frame| {
            (
                frame["seqNo"].as_i64().unwrap(),
                frame["tbc"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(shape, [(0, true), (1, true), (2, false)]);
    for frame in &plan.frames {
        assert_eq!(frame["requestId"], 7001);
        assert_valid("NotifyMonitoringReportRequest", frame);
    }
    assert_eq!(
        listed(&plan),
        [4003, 4030, 4031, 4032, 4033, 4034, 4035, 4036, 4037]
    );
    assert_eq!(
        (plan.planned, plan.before_reply, plan.disconnect_after),
        (3, false, None)
    );
    // Monitors of one variable share a MonitoringData entry within a fragment.
    let second = plan.frames[1]["monitor"].as_array().unwrap();
    assert_eq!(second.len(), 3);
    assert_eq!(second[1]["variableMonitoring"].as_array().unwrap().len(), 2);
}

#[test]
fn the_monitor_description_carries_every_stored_setting() {
    let station = station(json!({}));
    let mut replacement = item(&evse(), "Power", "UpperThreshold", 18000.0, 4);
    replacement["id"] = json!(4030);
    replacement["transaction"] = json!(true);
    set(&station, json!([replacement]));
    let (_, plan) = request(
        &station,
        &json!({"requestId": 1, "componentVariable": [{"component": evse(), "variable": {"name": "Power"}}]}),
    );
    let frame = &plan.unwrap().frames[0];
    assert_eq!(
        frame["monitor"][0]["variableMonitoring"][0],
        json!({"id": 4030, "transaction": true, "value": 18000.0, "type": "UpperThreshold", "severity": 4})
    );
}

#[test]
fn criteria_and_component_variables_limit_the_report() {
    let station = populated(json!({"monitors_per_fragment": 20}));
    let ids = |payload: Value| {
        request(&station, &payload)
            .1
            .map_or_else(Vec::new, |plan| listed(&plan))
    };
    // N02.FR.12-14.
    assert_eq!(
        ids(json!({"requestId": 1, "monitoringCriteria": ["ThresholdMonitoring"]})),
        [4003, 4030, 4031, 4032, 4036]
    );
    assert_eq!(
        ids(json!({"requestId": 1, "monitoringCriteria": ["DeltaMonitoring"]})),
        [4033, 4037]
    );
    assert_eq!(
        ids(json!({"requestId": 1, "monitoringCriteria": ["PeriodicMonitoring"]})),
        [4034, 4035]
    );
    assert_eq!(
        ids(
            json!({"requestId": 1, "monitoringCriteria": ["DeltaMonitoring", "PeriodicMonitoring"]})
        ),
        [4033, 4034, 4035, 4037]
    );
    // N02.FR.08 and N02.FR.16: a component without a variable selects every variable.
    assert_eq!(
        ids(json!({"requestId": 1, "componentVariable": [{"component": station_component()}]})),
        [4003, 4036, 4037]
    );
    // N02.FR.17: a variable without an instance selects every instance.
    assert_eq!(
        ids(
            json!({"requestId": 1, "componentVariable": [{"component": cabinet(), "variable": {"name": "temperature"}}]})
        ),
        [4034, 4035]
    );
    // N02.FR.19-21: an omitted instance or EVSE selects every one.
    assert_eq!(
        ids(
            json!({"requestId": 1, "componentVariable": [{"component": {"name": "TemperatureSensor"}}]})
        ),
        [4034, 4035]
    );
    assert_eq!(
        ids(json!({"requestId": 1, "componentVariable": [{"component": {"name": "Connector"}}]})),
        [4031, 4032]
    );
    assert_eq!(
        ids(
            json!({"requestId": 1, "componentVariable": [{"component": {"name": "Connector", "evse": {"id": 1}}}]})
        ),
        [4031, 4032]
    );
    assert_eq!(
        ids(
            json!({"requestId": 1, "componentVariable": [{"component": {"name": "Connector", "evse": {"id": 1, "connectorId": 2}}}]})
        ),
        Vec::<i64>::new()
    );
    // N02.FR.05: both filters apply together.
    let both = json!({"requestId": 1, "monitoringCriteria": ["DeltaMonitoring"], "componentVariable": [{"component": evse(), "variable": {"name": "Power"}}]});
    assert_eq!(ids(both), [4033]);
}

#[test]
fn an_empty_selection_and_unsupported_criteria_are_answered_without_fragments() {
    let station =
        populated(json!({"supported_criteria": ["ThresholdMonitoring", "DeltaMonitoring"]}));
    // N02.FR.10.
    let (reply, plan) = request(
        &station,
        &json!({"requestId": 1, "componentVariable": [{"component": {"name": "Heater"}}]}),
    );
    assert_eq!(
        (reply["status"].as_str(), plan),
        (Some("EmptyResultSet"), None)
    );
    // N02.FR.02.
    let (reply, plan) = request(
        &station,
        &json!({"requestId": 1, "monitoringCriteria": ["PeriodicMonitoring"]}),
    );
    assert_eq!(
        (reply["status"].as_str(), plan),
        (Some("NotSupported"), None)
    );
    let (reply, _) = request(&station, &json!({"requestId": 0}));
    assert_eq!(reply["status"], "Accepted", "a zero requestId is valid");
}

fn plan_with(fault: &str) -> ReportPlan201 {
    let station = populated(json!({"monitors_per_fragment": 3, "report_fault": fault}));
    request(&station, &json!({"requestId": 9})).1.unwrap()
}

fn sequence(plan: &ReportPlan201) -> Vec<i64> {
    plan.frames
        .iter()
        .map(|frame| frame["seqNo"].as_i64().unwrap())
        .collect()
}

#[test]
fn report_faults_break_exactly_one_thing() {
    assert_eq!(sequence(&plan_with("none")), [0, 1, 2]);
    assert_eq!(sequence(&plan_with("skip_seq_no")), [0, 2, 3]);
    assert_eq!(sequence(&plan_with("duplicate_seq_no")), [0, 0, 1]);
    let omitted = plan_with("omit_final_fragment");
    assert_eq!((sequence(&omitted), omitted.planned), (vec![0, 1], 3));
    assert_eq!(omitted.frames[1]["tbc"], true, "the chain never ends");
    let disconnect = plan_with("disconnect_mid_report");
    assert_eq!(
        (disconnect.frames.len(), disconnect.disconnect_after),
        (3, Some(1))
    );
    assert!(plan_with("fragments_before_reply").before_reply);
}

#[test]
fn a_single_fragment_report_disconnects_before_its_only_fragment() {
    let station = station(json!({"report_fault": "disconnect_mid_report"}));
    let plan = request(&station, &json!({"requestId": 2})).1.unwrap();
    assert_eq!((plan.planned, plan.disconnect_after), (1, Some(0)));
}

#[test]
fn a_set_reply_fault_breaks_the_result_count_or_the_component() {
    let items = json!([
        item(&connector(), "Temperature", "UpperThreshold", 75.0, 3),
        item(&evse(), "Power", "Delta", 1.0, 6)
    ]);
    let wrong = station(json!({"set_reply_fault": "wrong_result_count"}));
    let reply = wrong
        .handle_call(
            "SetVariableMonitoring",
            &json!({"setMonitoringData": items}),
        )
        .payload;
    assert_eq!(
        reply["setMonitoringResult"].as_array().unwrap().len(),
        3,
        "N04.FR.01"
    );
    let mismatched = station(json!({"set_reply_fault": "mismatched_component"}));
    let reply = mismatched
        .handle_call(
            "SetVariableMonitoring",
            &json!({"setMonitoringData": items}),
        )
        .payload;
    assert_ne!(
        reply["setMonitoringResult"][0]["component"], items[0]["component"],
        "N04.FR.02"
    );
    assert_valid("SetVariableMonitoringResponse", &reply);
    let _ = call(&mismatched, "SetMonitoringLevel", &json!({"severity": 1}));
}
