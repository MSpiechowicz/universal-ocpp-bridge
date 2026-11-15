//! Events raised by monitors: thresholds, deltas, periodic monitors, levels and bursts.
use crate::monitoring201_support::{
    cabinet, connector, drain_events, evse, item, set, station, station_component,
};
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};
use uob_sim::monitoring201::{EventOutcome201, Monitoring201Handle};

fn variable(name: &str) -> Value {
    json!({"name": name})
}

fn event(message: &Value, index: usize) -> &Value {
    &message["eventData"][index]
}

fn set_value(station: &Monitoring201Handle, component: &Value, name: &str, value: &str) -> usize {
    station
        .set_value(component, &variable(name), value)
        .unwrap()
}

fn monitor(
    station: &Monitoring201Handle,
    component: &Value,
    name: &str,
    kind: &str,
    value: f64,
    severity: i64,
) -> i64 {
    let results = set(
        station,
        json!([item(component, name, kind, value, severity)]),
    );
    results[0]["id"]
        .as_i64()
        .unwrap_or_else(|| panic!("{results:?}"))
}

#[test]
fn a_threshold_alerts_when_exceeded_and_reports_cleared_on_return() {
    let station = station(json!({}));
    let id = monitor(
        &station,
        &connector(),
        "Temperature",
        "UpperThreshold",
        75.0,
        3,
    );
    assert_eq!(
        set_value(&station, &connector(), "Temperature", "75"),
        0,
        "N07.FR.16: not on the threshold"
    );
    assert_eq!(set_value(&station, &connector(), "Temperature", "80"), 1);
    assert_eq!(
        set_value(&station, &connector(), "Temperature", "81"),
        0,
        "still exceeded"
    );
    assert_eq!(set_value(&station, &connector(), "Temperature", "70"), 1);
    let sent = drain_events(&station);
    assert_eq!(sent.len(), 2);
    let alert = event(&sent[0], 0);
    assert_eq!(
        (
            &alert["trigger"],
            &alert["actualValue"],
            alert.get("cleared")
        ),
        (&json!("Alerting"), &json!("80"), None)
    );
    // N07.FR.02: the return to normal is the same monitor with cleared=true.
    let cleared = event(&sent[1], 0);
    assert_eq!(
        (
            &cleared["trigger"],
            &cleared["cleared"],
            &cleared["actualValue"]
        ),
        (&json!("Alerting"), &json!(true), &json!("70"))
    );
    for message in &sent {
        // N07.FR.06/07: component, variable and monitor id; the first message has seqNo 0.
        assert_eq!(message["seqNo"], 0);
        assert_eq!(event(message, 0)["variableMonitoringId"], id);
        assert_eq!(event(message, 0)["component"], connector());
        assert_eq!(event(message, 0)["variable"], variable("Temperature"));
        assert_eq!(event(message, 0)["eventNotificationType"], "CustomMonitor");
    }
    assert_ne!(event(&sent[0], 0)["eventId"], event(&sent[1], 0)["eventId"]);
}

#[test]
fn a_lower_threshold_alerts_when_the_value_drops_below_it() {
    let station = station(json!({}));
    monitor(
        &station,
        &station_component(),
        "SupplyVoltage",
        "LowerThreshold",
        207.0,
        4,
    );
    assert_eq!(
        set_value(&station, &station_component(), "SupplyVoltage", "207"),
        0
    );
    assert_eq!(
        set_value(&station, &station_component(), "SupplyVoltage", "200"),
        1
    );
    assert_eq!(
        set_value(&station, &station_component(), "SupplyVoltage", "230"),
        1
    );
    let sent = drain_events(&station);
    assert_eq!(event(&sent[1], 0)["cleared"], true);
}

#[test]
fn a_new_threshold_is_judged_against_the_current_value_at_once() {
    let station = station(json!({}));
    set_value(&station, &connector(), "Temperature", "90");
    monitor(
        &station,
        &connector(),
        "Temperature",
        "UpperThreshold",
        75.0,
        3,
    );
    let sent = drain_events(&station);
    assert_eq!(sent.len(), 1, "N07.FR.16 and N07.FR.11");
    assert_eq!(event(&sent[0], 0)["actualValue"], "90");
    // Raising the threshold above the value clears the old one (N07.FR.11).
    let mut replace = item(&connector(), "Temperature", "UpperThreshold", 95.0, 3);
    replace["id"] = json!(4031);
    set(&station, json!([replace]));
    assert_eq!(event(&drain_events(&station)[0], 0)["cleared"], true);
}

#[test]
fn removing_an_exceeded_threshold_sends_no_cleared_event() {
    let station = station(json!({}));
    let id = monitor(
        &station,
        &connector(),
        "Temperature",
        "UpperThreshold",
        75.0,
        3,
    );
    set_value(&station, &connector(), "Temperature", "80");
    drain_events(&station);
    let _ = station.handle_call("ClearVariableMonitoring", &json!({"id": [id]}));
    assert_eq!(
        set_value(&station, &connector(), "Temperature", "60"),
        0,
        "N07.FR.12"
    );
    assert!(drain_events(&station).is_empty());
}

#[test]
fn a_delta_monitor_fires_after_a_change_of_more_than_its_value() {
    let station = station(json!({}));
    monitor(&station, &evse(), "Power", "Delta", 5.0, 6);
    assert_eq!(
        set_value(&station, &evse(), "Power", "5"),
        0,
        "N07.FR.18: more than, not equal"
    );
    assert_eq!(set_value(&station, &evse(), "Power", "6"), 1);
    assert_eq!(
        set_value(&station, &evse(), "Power", "10"),
        0,
        "measured from the last notice"
    );
    assert_eq!(set_value(&station, &evse(), "Power", "0"), 1);
    let sent = drain_events(&station);
    assert!(
        sent.iter()
            .all(|message| event(message, 0)["trigger"] == "Delta")
    );
    assert_eq!(event(&sent[1], 0)["actualValue"], "0");
}

#[test]
fn a_delta_monitor_on_a_non_numeric_variable_fires_on_every_change() {
    let station = station(json!({}));
    let component = json!({"name": "Connector", "evse": {"id": 2, "connectorId": 1}});
    monitor(&station, &component, "AvailabilityState", "Delta", 1.0, 6);
    assert_eq!(
        set_value(&station, &component, "AvailabilityState", "Unavailable"),
        1
    );
    assert_eq!(
        set_value(&station, &component, "AvailabilityState", "Unavailable"),
        0
    );
    assert_eq!(
        set_value(&station, &component, "AvailabilityState", "Available"),
        1,
        "N07.FR.19"
    );
}

#[test]
fn monitors_report_their_own_origin() {
    let station = station(json!({}));
    set_value(&station, &station_component(), "SupplyVoltage", "290");
    set_value(&station, &evse(), "Power", "21000");
    let sent = drain_events(&station);
    let types: Vec<_> = sent
        .iter()
        .map(|message| event(message, 0)["eventNotificationType"].clone())
        .collect();
    assert_eq!(
        types,
        [json!("HardWiredMonitor"), json!("PreconfiguredMonitor")]
    );
    assert_eq!(event(&sent[0], 0)["variableMonitoringId"], 4003);
}

#[test]
fn periodic_monitors_fire_when_their_interval_passed() {
    let station = station(json!({}));
    monitor(&station, &cabinet(), "Temperature", "Periodic", 60.0, 9);
    let now = OffsetDateTime::now_utc();
    station.advance(now + Duration::seconds(30));
    assert!(drain_events(&station).is_empty());
    station.advance(now + Duration::seconds(61));
    let sent = drain_events(&station);
    assert_eq!(sent.len(), 1, "N08.FR.06");
    assert_eq!(
        (&event(&sent[0], 0)["trigger"], &sent[0]["seqNo"]),
        (&json!("Periodic"), &json!(0))
    );
    station.advance(now + Duration::seconds(62));
    assert!(
        drain_events(&station).is_empty(),
        "the next period starts when it fired"
    );
    station.advance(now + Duration::seconds(125));
    assert_eq!(drain_events(&station).len(), 1);
}

#[test]
fn clock_aligned_monitors_fire_on_the_next_aligned_instant() {
    let station = station(json!({}));
    monitor(
        &station,
        &cabinet(),
        "Temperature",
        "PeriodicClockAligned",
        900.0,
        8,
    );
    let aligned = (OffsetDateTime::now_utc().unix_timestamp() / 900 + 1) * 900;
    let at = OffsetDateTime::from_unix_timestamp(aligned).unwrap();
    station.advance(at - Duration::seconds(1));
    assert!(drain_events(&station).is_empty());
    station.advance(at);
    assert_eq!(drain_events(&station).len(), 1, "N08.FR.07");
}

#[test]
fn events_above_the_monitoring_level_are_not_sent() {
    let station = station(json!({}));
    monitor(
        &station,
        &connector(),
        "Temperature",
        "UpperThreshold",
        75.0,
        5,
    );
    let _ = station.handle_call("SetMonitoringLevel", &json!({"severity": 4}));
    assert_eq!(set_value(&station, &connector(), "Temperature", "80"), 0);
    assert!(drain_events(&station).is_empty(), "N07.FR.15");
    assert_eq!(station.snapshot()["eventsSuppressed"], 1);
    // The monitor still tracks the situation, so the level change is not a missed alert.
    let _ = station.handle_call("SetMonitoringLevel", &json!({"severity": 5}));
    assert_eq!(set_value(&station, &connector(), "Temperature", "70"), 1);
    assert_eq!(event(&drain_events(&station)[0], 0)["cleared"], true);
}

#[test]
fn a_burst_is_chained_over_messages_with_tbc() {
    let station = station(json!({"events_per_message": 2}));
    let id = monitor(
        &station,
        &connector(),
        "Temperature",
        "UpperThreshold",
        75.0,
        3,
    );
    assert_eq!(
        station.emit_events(
            &connector(),
            &variable("Temperature"),
            5,
            Some(i32::try_from(id).unwrap())
        ),
        Ok(5)
    );
    let sent = drain_events(&station);
    let shape: Vec<_> = sent
        .iter()
        .map(|message| {
            (
                message["seqNo"].as_i64().unwrap(),
                message["tbc"].as_bool().unwrap(),
                message["eventData"].as_array().unwrap().len(),
            )
        })
        .collect();
    assert_eq!(shape, [(0, true, 2), (1, true, 2), (2, false, 1)]);
    let mut event_ids: Vec<_> = sent
        .iter()
        .flat_map(|message| message["eventData"].as_array().unwrap())
        .map(|data| data["eventId"].as_i64().unwrap())
        .collect();
    event_ids.sort_unstable();
    event_ids.dedup();
    assert_eq!(event_ids.len(), 5, "every event has its own eventId");
    assert_eq!(station.snapshot()["eventsEmitted"], 5);
    // Bursts need a monitor on the variable.
    assert_eq!(
        station.emit_events(&cabinet(), &variable("Temperature"), 1, None),
        Err("monitor_not_found")
    );
    assert_eq!(
        station.emit_events(&cabinet(), &variable("Nothing"), 1, None),
        Err("monitored_variable_unknown")
    );
}

#[test]
fn csms_answers_are_recorded_and_failed_sends_are_retried() {
    let station = station(json!({}));
    monitor(
        &station,
        &connector(),
        "Temperature",
        "UpperThreshold",
        75.0,
        3,
    );
    for value in ["80", "70", "90"] {
        set_value(&station, &connector(), "Temperature", value);
    }
    let now = OffsetDateTime::now_utc();
    let first = station.next_event(now).unwrap();
    assert!(
        station.next_event(now).is_none(),
        "one NotifyEvent in flight"
    );
    station.event_finished(&first, EventOutcome201::Failed, now);
    assert!(station.next_event(now).is_none(), "retry waits");
    let retry = station.next_event(now + Duration::seconds(2)).unwrap();
    assert_eq!(retry, first, "the lost message is resent unchanged");
    station.event_finished(&retry, EventOutcome201::Acknowledged, now);
    let second = station.next_event(now).unwrap();
    station.event_finished(&second, EventOutcome201::CallError, now);
    let snapshot = station.snapshot();
    assert_eq!(
        (&snapshot["eventsAcknowledged"], &snapshot["eventsRejected"]),
        (&json!(1), &json!(1))
    );
    assert_eq!(snapshot["lastEventOutcome"], "CallError");
    assert_eq!(snapshot["eventsQueued"], 1);
}

#[test]
fn invalid_values_do_not_change_the_variable() {
    let station = station(json!({}));
    assert_eq!(
        station.set_value(&connector(), &variable("Temperature"), "hot"),
        Err("monitored_value_invalid")
    );
    assert_eq!(
        station.set_value(&station_component(), &variable("Tamper"), "maybe"),
        Err("monitored_value_invalid")
    );
    assert_eq!(
        station.set_value(&connector(), &variable("Nothing"), "1"),
        Err("monitored_variable_unknown")
    );
    assert_eq!(station.snapshot()["variables"][0]["value"], "40");
}
