use super::*;
use serde_json::json;
use uob_contracts::{DeviceEvse201, MonitorSeverity201};

fn query(
    criteria: Vec<MonitoringCriterion201>,
    selectors: Vec<DeviceSelector201>,
) -> MonitoringQuery201 {
    MonitoringQuery201::GetMonitoringReport {
        request_id: 1,
        criteria,
        component_variables: selectors,
    }
}

fn selector(component: Value, variable: Option<Value>) -> DeviceSelector201 {
    DeviceSelector201 {
        component: serde_json::from_value(component).unwrap(),
        variable: variable.map(|variable| serde_json::from_value(variable).unwrap()),
    }
}

fn payload(component: Value, variable: Value, kind: &str) -> Value {
    let mut payload = json!({
        "requestId": 1, "seqNo": 3, "tbc": true, "generatedAt": "2026-11-10T09:15:00Z",
        "monitor": [{
            "variableMonitoring": [{"id": 9, "transaction": true, "value": 2.50, "type": kind, "severity": 4}],
        }],
    });
    payload["monitor"][0]["component"] = component;
    payload["monitor"][0]["variable"] = variable;

    payload
}

fn allowed(query: &MonitoringQuery201, component: Value, variable: Value, kind: &str) -> bool {
    match fragment(&payload(component, variable, kind), query) {
        Ok(_) => true,
        Err(ReportFailure::CorrelationMismatch) => false,
        Err(other) => panic!("unexpected failure {other:?}"),
    }
}

#[test]
fn a_fragment_maps_to_exact_monitors_with_its_native_sequence() {
    let sanitized = fragment(
        &payload(
            json!({"name": "EVSE", "evse": {"id": 1}}),
            json!({"name": "Power"}),
            "Delta",
        ),
        &query(vec![], vec![]),
    )
    .ok()
    .unwrap();
    assert_eq!(
        (
            sanitized.metadata.sequence,
            sanitized.metadata.more,
            sanitized.metadata.monitors
        ),
        (3, true, 1)
    );
    let monitor: ReportedMonitor201 = serde_json::from_slice(&sanitized.items[0]).unwrap();
    assert_eq!(monitor.id, 9);
    assert!(monitor.transaction);
    // Exact decimal text, never binary floating point.
    assert_eq!(monitor.value.to_string(), "2.5");
    assert_eq!(monitor.severity, MonitorSeverity201::new(4).unwrap());
    assert_eq!(
        monitor.component.evse,
        Some(DeviceEvse201 {
            id: 1,
            connector_id: None
        })
    );
}

#[test]
fn criteria_are_or_combined_and_map_the_native_monitor_types() {
    let station = json!({"name": "ChargingStation"});
    let variable = json!({"name": "Power"});
    let threshold = query(vec![MonitoringCriterion201::ThresholdMonitoring], vec![]);
    for (kind, expected) in [
        ("UpperThreshold", true),
        ("LowerThreshold", true),
        ("Delta", false),
        ("Periodic", false),
        ("PeriodicClockAligned", false),
    ] {
        assert_eq!(
            allowed(&threshold, station.clone(), variable.clone(), kind),
            expected,
            "{kind}"
        );
    }
    let either = query(
        vec![
            MonitoringCriterion201::DeltaMonitoring,
            MonitoringCriterion201::PeriodicMonitoring,
        ],
        vec![],
    );
    for (kind, expected) in [
        ("Delta", true),
        ("Periodic", true),
        ("PeriodicClockAligned", true),
        ("UpperThreshold", false),
    ] {
        assert_eq!(
            allowed(&either, station.clone(), variable.clone(), kind),
            expected,
            "{kind}"
        );
    }
}

/// Whether a `Delta` monitor on `component`/`variable` is inside a one-selector request.
fn selected(wanted: (Value, Option<Value>), component: Value, variable: Value) -> bool {
    let request = query(vec![], vec![selector(wanted.0, wanted.1)]);

    allowed(&request, component, variable, "Delta")
}

#[test]
fn a_selector_without_variable_or_instance_is_a_wildcard() {
    let station = || (json!({"name": "ChargingStation"}), None);
    // A component without variable selects every variable (N02.FR.16).
    assert!(selected(
        station(),
        json!({"name": "chargingstation"}),
        json!({"name": "Anything"})
    ));
    assert!(!selected(
        station(),
        json!({"name": "EVSE"}),
        json!({"name": "Power"})
    ));

    // A variable without instance selects every instance (N02.FR.17); with one it is exact.
    let any_instance = || (json!({"name": "A"}), Some(json!({"name": "Temperature"})));
    assert!(selected(
        any_instance(),
        json!({"name": "A"}),
        json!({"name": "TEMPERATURE", "instance": "Inlet"})
    ));
    let inlet = || {
        (
            json!({"name": "A"}),
            Some(json!({"name": "Temperature", "instance": "Inlet"})),
        )
    };
    assert!(selected(
        inlet(),
        json!({"name": "A"}),
        json!({"name": "Temperature", "instance": "INLET"})
    ));
    assert!(!selected(
        inlet(),
        json!({"name": "A"}),
        json!({"name": "Temperature", "instance": "Outlet"})
    ));
    assert!(!selected(
        inlet(),
        json!({"name": "A"}),
        json!({"name": "Temperature"})
    ));
}

#[test]
fn component_instance_and_evse_scope_narrow_only_when_given() {
    let power = || json!({"name": "Power"});
    let cabinet = || (json!({"name": "Sensor", "instance": "Cabinet"}), None);
    assert!(selected(
        cabinet(),
        json!({"name": "SENSOR", "instance": "cabinet"}),
        power()
    ));
    assert!(!selected(
        cabinet(),
        json!({"name": "Sensor", "instance": "Roof"}),
        power()
    ));
    assert!(!selected(cabinet(), json!({"name": "Sensor"}), power()));

    let evse = || (json!({"name": "EVSE", "evse": {"id": 1}}), None);
    assert!(selected(
        evse(),
        json!({"name": "EVSE", "evse": {"id": 1}}),
        power()
    ));
    assert!(selected(
        evse(),
        json!({"name": "EVSE", "evse": {"id": 1, "connectorId": 2}}),
        power()
    ));
    assert!(!selected(
        evse(),
        json!({"name": "EVSE", "evse": {"id": 2}}),
        power()
    ));
    assert!(!selected(evse(), json!({"name": "EVSE"}), power()));

    let connector = || {
        (
            json!({"name": "Connector", "evse": {"id": 1, "connectorId": 1}}),
            None,
        )
    };
    assert!(selected(
        connector(),
        json!({"name": "Connector", "evse": {"id": 1, "connectorId": 1}}),
        power()
    ));
    assert!(!selected(
        connector(),
        json!({"name": "Connector", "evse": {"id": 1, "connectorId": 2}}),
        power()
    ));
}

#[test]
fn selectors_compare_names_with_full_unicode_case_folding() {
    let folded = (json!({"name": "Straße"}), None);
    assert!(selected(
        folded,
        json!({"name": "STRASSE"}),
        json!({"name": "V"})
    ));
}

#[test]
fn criteria_and_selectors_must_both_admit_a_monitor() {
    let both = query(
        vec![MonitoringCriterion201::DeltaMonitoring],
        vec![selector(
            json!({"name": "EVSE", "evse": {"id": 1}}),
            Some(json!({"name": "Power"})),
        )],
    );
    let evse = json!({"name": "EVSE", "evse": {"id": 1}});
    assert!(allowed(
        &both,
        evse.clone(),
        json!({"name": "Power"}),
        "Delta"
    ));
    assert!(!allowed(
        &both,
        evse.clone(),
        json!({"name": "Power"}),
        "Periodic"
    ));
    assert!(!allowed(&both, evse, json!({"name": "Voltage"}), "Delta"));
}

#[test]
fn content_the_contract_cannot_carry_is_invalid_never_dropped() {
    let any = query(vec![], vec![]);
    let component = json!({"name": "A"});
    let variable = json!({"name": "B"});
    let invalid = |change: &dyn Fn(&mut Value)| {
        let mut value = payload(component.clone(), variable.clone(), "Delta");
        change(&mut value);
        matches!(fragment(&value, &any), Err(ReportFailure::InvalidFragment))
    };
    assert!(invalid(
        &|p| p["monitor"][0]["variableMonitoring"][0]["severity"] = json!(10)
    ));
    assert!(invalid(
        &|p| p["monitor"][0]["variableMonitoring"][0]["id"] = json!(2_147_483_648_i64)
    ));
    assert!(invalid(
        &|p| p["monitor"][0]["component"]["name"] = json!("a\u{7}")
    ));
    assert!(invalid(
        &|p| p["monitor"][0]["component"]["evse"] = json!({"id": 0})
    ));
    assert!(invalid(&|p| p["generatedAt"] = json!("yesterday")));
    assert!(invalid(&|p| p["seqNo"] = json!(-1)));
    // A value that is no exact plain decimal within bounds.
    for lexeme in ["1e999", "1e-999"] {
        let text = payload(component.clone(), variable.clone(), "Delta")
            .to_string()
            .replace("\"value\":2.5", &format!("\"value\":{lexeme}"));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert!(
            matches!(fragment(&value, &any), Err(ReportFailure::InvalidFragment)),
            "{lexeme}"
        );
    }
    // A request that is not a report request never owns a fragment.
    let level = MonitoringQuery201::SetMonitoringLevel {
        severity: MonitorSeverity201::new(1).unwrap(),
    };
    assert!(matches!(
        fragment(&payload(component, variable, "Delta"), &level),
        Err(ReportFailure::CorrelationMismatch)
    ));
}

#[test]
fn a_fragment_without_monitors_is_valid_and_empty() {
    let empty = json!({"requestId": 1, "seqNo": 0, "generatedAt": "2026-11-10T09:15:00Z"});
    let sanitized = fragment(&empty, &query(vec![], vec![])).ok().unwrap();
    assert!(sanitized.items.is_empty());
    assert_eq!(
        (sanitized.metadata.more, sanitized.metadata.monitors),
        (false, 0)
    );
}
