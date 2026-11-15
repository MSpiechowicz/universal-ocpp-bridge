//! Charger-initiated `NotifyEvent` (N07, N08): decoded into an observed fact and answered with the
//! empty native response.
use crate::harness::Harness;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::timeout;
use uob_application::{ChargerObservation, NotifyEventObservation201};
use uob_contracts::*;
use uob_protocol_adapter::{
    DecodeErrorKind,
    v201::{decode_call, monitoring_event},
};

fn corpus(name: &str) -> Value {
    crate::harness::corpus(name)
}

fn frame(payload: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!([2, "event-1", "NotifyEvent", payload])).unwrap()
}

fn decode(payload: &Value) -> NotifyEventObservation201 {
    let decoded = decode_call(&frame(payload)).unwrap();
    assert_eq!(decoded.action.as_str(), "NotifyEvent");
    assert_eq!(decoded.message_id, "event-1");
    let ChargerObservation::MonitoringEvents201(observation) = decoded.observation else {
        panic!("monitoring events");
    };
    observation
}

fn refused(payload: &Value) {
    let error = decode_call(&frame(payload)).unwrap_err();
    assert_eq!(error.kind(), DecodeErrorKind::InvalidPayload, "{payload}");
}

fn time(text: &str) -> UtcTimestamp {
    serde_json::from_value(json!(text)).unwrap()
}

#[test]
fn an_alerting_event_maps_every_native_field() {
    let observation = decode(&corpus("event-alerting"));
    assert_eq!(observation.generated_at, time("2026-11-10T10:00:00Z"));
    assert_eq!((observation.seq_no, observation.tbc), (0, false));
    let [event] = observation.events.as_slice() else {
        panic!("one event");
    };
    assert_eq!(
        *event,
        MonitoringEvent201 {
            event_id: 9001,
            timestamp: time("2026-11-10T10:00:00Z"),
            trigger: EventTrigger201::Alerting,
            cause: None,
            actual_value: "78.4".to_owned(),
            tech_code: None,
            tech_info: None,
            cleared: None,
            transaction_id: None,
            variable_monitoring_id: Some(4001),
            notification: EventNotification201::CustomMonitor,
            component: DeviceComponent201 {
                name: "Connector".to_owned(),
                instance: None,
                evse: Some(DeviceEvse201 {
                    id: 1,
                    connector_id: Some(1),
                }),
            },
            variable: DeviceVariable201 {
                name: "Temperature".to_owned(),
                instance: None,
            },
        }
    );
}

#[test]
fn optional_native_facts_survive_exactly() {
    let cleared = decode(&corpus("event-cleared")).events.remove(0);
    assert_eq!(cleared.cleared, Some(true));

    let delta = decode(&corpus("event-delta")).events.remove(0);
    assert_eq!(delta.trigger, EventTrigger201::Delta);
    assert_eq!(
        delta.transaction_id.as_deref(),
        Some("a7c3e9d2-5b41-4f60-9a1e-0c2d8e7f6b35")
    );

    // A hard-wired notification has no monitor behind it.
    let wired = decode(&corpus("event-hard-wired")).events.remove(0);
    assert_eq!(
        wired.notification,
        EventNotification201::HardWiredNotification
    );
    assert_eq!(wired.variable_monitoring_id, None);
    assert_eq!(wired.tech_code.as_deref(), Some("OTP"));
    assert_eq!(wired.tech_info.as_deref(), Some("thermal cut-out opened"));

    let preconfigured = decode(&corpus("event-preconfigured")).events.remove(0);
    assert_eq!(
        preconfigured.notification,
        EventNotification201::PreconfiguredMonitor
    );
    let periodic = decode(&corpus("event-periodic")).events.remove(0);
    assert_eq!(periodic.trigger, EventTrigger201::Periodic);

    // A write-only variable reports an empty value (N07.FR.10).
    let secret = decode(&corpus("event-write-only")).events.remove(0);
    assert_eq!(secret.actual_value, "");

    // Maximum native text lengths are accepted whole.
    let longest = decode(&corpus("event-max-lengths")).events.remove(0);
    assert_eq!(longest.actual_value.chars().count(), 2500);
    assert_eq!(
        longest.tech_code.as_ref().map(|text| text.chars().count()),
        Some(50)
    );
    assert_eq!(
        longest.tech_info.as_ref().map(|text| text.chars().count()),
        Some(500)
    );
    assert_eq!(
        longest
            .transaction_id
            .as_ref()
            .map(|text| text.chars().count()),
        Some(36)
    );
}

#[test]
fn a_burst_keeps_its_sequence_and_continuation_flags() {
    let observed: Vec<_> = ["event-burst-0", "event-burst-1", "event-burst-2"]
        .into_iter()
        .map(|name| {
            let payload = corpus(name);
            let observation = decode(&payload);
            assert_eq!(
                observation.tbc,
                payload["tbc"].as_bool().unwrap_or(false),
                "{name}"
            );
            (
                observation.seq_no,
                observation.tbc,
                observation.events.len(),
            )
        })
        .collect();
    assert_eq!(observed, [(0, true, 2), (1, true, 1), (2, false, 1)]);
    let second = decode(&corpus("event-burst-1")).events.remove(0);
    assert_eq!(second.cause, Some(9010));

    // The application gate bounds records, not the decoder: every event of the CALL is kept.
    let mut many = corpus("event-alerting");
    let event = many["eventData"][0].clone();
    many["eventData"] = Value::Array(vec![event; 40]);
    assert_eq!(decode(&many).events.len(), 40);
}

#[test]
fn custom_data_is_never_retained() {
    let plain = decode(&corpus("event-alerting"));
    let mut custom = corpus("event-alerting");
    custom["customData"] = json!({"vendorId": "v", "secret": "request"});
    custom["eventData"][0]["customData"] = json!({"vendorId": "v", "secret": "event"});
    custom["eventData"][0]["component"]["customData"] = json!({"vendorId": "v"});
    custom["eventData"][0]["component"]["evse"]["customData"] = json!({"vendorId": "v"});
    custom["eventData"][0]["variable"]["customData"] = json!({"vendorId": "v"});
    assert_eq!(decode(&custom), plain);
}

#[test]
fn every_remaining_negative_corpus_case_is_refused_except_ordering_which_belongs_to_the_gate() {
    const CASES: &str = include_str!(
        "../../../../tests/ocpp-fixtures/corpus/wire/2.0.1/monitoring-negative-cases.json"
    );
    let cases: Value = serde_json::from_str(CASES).unwrap();
    let mut refused_count = 0;
    let mut ordering = 0;
    for case in cases["cases"].as_array().unwrap() {
        if case["wire"][0] != 2 || case["wire"][2] != "NotifyEvent" {
            continue;
        }
        let payload = &case["wire"][3];
        if case["rule"] == "fragment-sequence" {
            // Gaps and restarts are recorded as evidence by the gate, never refused here.
            decode(payload);
            ordering += 1;
        } else {
            refused(payload);
            refused_count += 1;
        }
    }
    assert!(
        refused_count == 23 && ordering == 3,
        "{refused_count} {ordering}"
    );
}

#[test]
fn text_and_integers_beyond_what_the_record_can_hold_are_refused() {
    let mut payload = corpus("event-alerting");
    payload["eventData"][0]["actualValue"] = json!("x".repeat(2501));
    refused(&payload);
    payload["eventData"][0]["actualValue"] = json!("é".repeat(2500));
    assert_eq!(
        decode(&payload).events[0].actual_value.chars().count(),
        2500
    );
    payload["eventData"][0]["actualValue"] = json!("é".repeat(2501));
    refused(&payload);

    for (field, value) in [
        ("techCode", json!("c".repeat(51))),
        ("techInfo", json!("i".repeat(501))),
        ("transactionId", json!("t".repeat(37))),
        ("eventId", json!(2_147_483_648_i64)),
        ("cause", json!(-2_147_483_649_i64)),
        ("variableMonitoringId", json!(2_147_483_648_i64)),
    ] {
        let mut payload = corpus("event-alerting");
        payload["eventData"][0][field] = value;
        refused(&payload);
    }

    // EVSE and connector identities are positive.
    let mut payload = corpus("event-alerting");
    payload["eventData"][0]["component"]["evse"]["id"] = json!(0);
    refused(&payload);
    // The sequence number is an unsigned 32-bit value.
    let mut payload = corpus("event-alerting");
    payload["seqNo"] = json!(4_294_967_296_i64);
    refused(&payload);
}

/// The journal-bound observation of one decoded event, whatever the CALL looked like.
fn only_event(payload: &Value) -> MonitoringEvent201 {
    let mut events = decode(payload).events;
    assert_eq!(events.len(), 1);
    events.remove(0)
}

#[test]
fn control_characters_in_charger_text_are_replaced_never_refused() {
    let mut payload = corpus("event-alerting");
    payload["eventData"][0]["actualValue"] = json!("7\t8\n.4\u{0}");
    payload["eventData"][0]["techCode"] = json!("E\u{1b}1");
    payload["eventData"][0]["techInfo"] = json!("tripped\r\nopen\u{7f}\u{85}");
    payload["eventData"][0]["transactionId"] = json!("tx\u{0}1");
    payload["eventData"][0]["component"]["name"] = json!("Con\u{1b}nector");
    payload["eventData"][0]["component"]["instance"] = json!("a\tb");
    payload["eventData"][0]["variable"]["name"] = json!("Temp\nerature");
    payload["eventData"][0]["variable"]["instance"] = json!("\u{0}");
    let event = only_event(&payload);

    assert_eq!(event.actual_value, "7\u{fffd}8\u{fffd}.4\u{fffd}");
    assert_eq!(event.tech_code.as_deref(), Some("E\u{fffd}1"));
    assert_eq!(
        event.tech_info.as_deref(),
        Some("tripped\u{fffd}\u{fffd}open\u{fffd}\u{fffd}")
    );
    assert_eq!(event.transaction_id.as_deref(), Some("tx\u{fffd}1"));
    assert_eq!(event.component.name, "Con\u{fffd}nector");
    assert_eq!(event.component.instance.as_deref(), Some("a\u{fffd}b"));
    assert_eq!(event.variable.name, "Temp\u{fffd}erature");
    assert_eq!(event.variable.instance.as_deref(), Some("\u{fffd}"));

    // Native facts around the text are untouched, and no control character remains anywhere.
    assert_eq!(event.event_id, 9001);
    assert_eq!(event.variable_monitoring_id, Some(4001));
    assert!(!serde_json::to_string(&event).unwrap().contains("\\u00"));

    // The schema length limits are in characters, so replacement never changes them.
    let mut longest = corpus("event-max-lengths");
    longest["eventData"][0]["actualValue"] = json!("\t".repeat(2500));
    longest["eventData"][0]["techInfo"] = json!("\n".repeat(500));
    let event = only_event(&longest);
    assert_eq!(event.actual_value, "\u{fffd}".repeat(2500));
    assert_eq!(event.tech_info, Some("\u{fffd}".repeat(500)));
}

#[test]
fn an_event_without_its_monitor_id_is_accepted_for_every_notification_type() {
    for notification in [
        "HardWiredNotification",
        "HardWiredMonitor",
        "PreconfiguredMonitor",
        "CustomMonitor",
    ] {
        let mut payload = corpus("event-alerting");
        payload["eventData"][0]
            .as_object_mut()
            .unwrap()
            .remove("variableMonitoringId");
        payload["eventData"][0]["eventNotificationType"] = json!(notification);
        let event = only_event(&payload);
        assert_eq!(event.variable_monitoring_id, None, "{notification}");
        assert_eq!(
            event.notification,
            serde_json::from_value(json!(notification)).unwrap()
        );
    }
}

#[test]
fn tolerated_corpus_fixtures_are_accepted_and_sanitized() {
    let missing = only_event(&corpus("event-no-monitor-id"));
    assert_eq!(missing.notification, EventNotification201::CustomMonitor);
    assert_eq!(missing.variable_monitoring_id, None);

    let tab_and_newline = only_event(&corpus("event-control-characters"));
    assert_eq!(
        tab_and_newline.tech_info.as_deref(),
        Some("tripped\u{fffd}open\u{fffd}\u{fffd}closed")
    );
    assert_eq!(tab_and_newline.actual_value, "78\u{fffd}4");
}

#[test]
fn the_native_response_is_the_empty_object() {
    assert_eq!(monitoring_event::response(), json!({}));
}

#[tokio::test]
async fn an_event_crosses_the_socket_and_is_answered_with_the_empty_response() {
    let mut h = Harness::new().await;
    let payload = corpus("event-alerting");
    h.running
        .peer
        .send_text(json!([2, "ev-wire-1", "NotifyEvent", payload]).to_string())
        .await
        .unwrap();
    let incoming = timeout(Duration::from_secs(2), h.running.outputs.incoming.receive())
        .await
        .expect("incoming event handoff")
        .unwrap();
    assert!(matches!(
        incoming.call.observation,
        ChargerObservation::MonitoringEvents201(_)
    ));
    incoming
        .responder
        .respond(&monitoring_event::response())
        .unwrap();
    let answer = crate::support::receive_json(&mut h.running.peer).await;
    assert_eq!(answer, json!([3, "ev-wire-1", {}]));

    // A control character or a missing monitor id never wedges the charger's queue.
    let mut sloppy = corpus("event-alerting");
    sloppy["eventData"][0]["techInfo"] = json!("tripped\topen");
    sloppy["eventData"][0]
        .as_object_mut()
        .unwrap()
        .remove("variableMonitoringId");
    h.running
        .peer
        .send_text(json!([2, "ev-wire-3", "NotifyEvent", sloppy]).to_string())
        .await
        .unwrap();
    let incoming = timeout(Duration::from_secs(2), h.running.outputs.incoming.receive())
        .await
        .expect("incoming event handoff")
        .unwrap();
    assert!(matches!(
        incoming.call.observation,
        ChargerObservation::MonitoringEvents201(_)
    ));
    incoming
        .responder
        .respond(&monitoring_event::response())
        .unwrap();
    assert_eq!(
        crate::support::receive_json(&mut h.running.peer).await,
        json!([3, "ev-wire-3", {}])
    );

    // A schema-invalid event never reaches the application.
    let mut invalid = corpus("event-alerting");
    invalid["eventData"][0]["trigger"] = json!("alerting");
    let answer = h.notify("ev-wire-2", "NotifyEvent", invalid).await;
    assert_eq!((&answer[0], &answer[1]), (&json!(4), &json!("ev-wire-2")));
    h.no_extra_call().await;
    h.close().await;
}
