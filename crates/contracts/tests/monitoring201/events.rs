use crate::support::*;
use serde::Deserialize;
use serde_json::json;
use uob_contracts::*;

fn event(id: i32) -> MonitoringEvent201 {
    MonitoringEvent201 {
        event_id: id,
        timestamp: timestamp("2026-10-08T10:15:00Z"),
        trigger: EventTrigger201::Alerting,
        cause: Some(id - 1),
        actual_value: "7500.5".to_owned(),
        tech_code: Some("E42".to_owned()),
        tech_info: Some("over limit".to_owned()),
        cleared: Some(false),
        transaction_id: Some("tx-126".to_owned()),
        variable_monitoring_id: Some(7),
        notification: EventNotification201::CustomMonitor,
        component: component(),
        variable: variable(),
    }
}

fn record(ordering: MonitoringEventOrdering201) -> MonitoringEventRecord201 {
    MonitoringEventRecord201 {
        generated_at: timestamp("2026-10-08T10:15:01Z"),
        seq_no: 4,
        tbc: true,
        ordering,
        events: vec![event(2), event(3)],
        events_omitted: 5,
        shed_calls: 2,
        shed_events: 9,
    }
}

#[test]
fn event_records_round_trip_as_distinct_station_events() {
    for ordering in [
        MonitoringEventOrdering201::InSequence,
        MonitoringEventOrdering201::Restarted,
        MonitoringEventOrdering201::Gap { expected: 3 },
    ] {
        let event = StationEvent::MonitoringEvents201 {
            station_snapshot_invalidated: station(),
            monitoring_events_201: record(ordering),
        };
        let encoded = serde_json::to_value(&event).unwrap();
        assert_eq!(encoded["station_snapshot_invalidated"], "station-126");
        assert!(encoded["monitoring_events_201"]["ordering"]["kind"].is_string());
        assert_eq!(
            serde_json::from_value::<StationEvent>(encoded).unwrap(),
            event
        );
    }

    let invalidation = StationEvent::Invalidation {
        station_snapshot_invalidated: station(),
    };
    assert_eq!(
        serde_json::from_value::<StationEvent>(serde_json::to_value(&invalidation).unwrap())
            .unwrap(),
        invalidation,
        "a plain marker never decodes as monitoring evidence"
    );
}

/// The station journal variants released before this contract addition.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
enum ReleasedStationEvent {
    StationSnapshot(StationSnapshot),
    Transaction(TransactionSnapshot),
    TriggerNotification {
        station_snapshot_invalidated: StationId,
        class: TriggerMessageClass,
        connector_id: Option<u32>,
        status: Option<String>,
    },
    TriggerNotification201 {
        station_snapshot_invalidated: StationId,
        trigger_class_201: TriggerMessageClass201,
        target: TriggerTarget201,
        status: Option<String>,
    },
    ChargingNegotiation201 {
        station_snapshot_invalidated: StationId,
        charging_negotiation_201: ChargingNegotiation201,
    },
    Invalidation {
        station_snapshot_invalidated: StationId,
    },
}

#[test]
fn released_readers_decode_monitoring_evidence_as_an_invalidation_marker() {
    let encoded = serde_json::to_value(StationEvent::MonitoringEvents201 {
        station_snapshot_invalidated: station(),
        monitoring_events_201: record(MonitoringEventOrdering201::InSequence),
    })
    .unwrap();
    match serde_json::from_value::<ReleasedStationEvent>(encoded).unwrap() {
        ReleasedStationEvent::Invalidation {
            station_snapshot_invalidated,
        } => assert_eq!(station_snapshot_invalidated, station()),
        other => panic!("released reader misread monitoring evidence: {other:?}"),
    }
}

#[test]
fn event_fields_are_bounded_by_the_native_schema() {
    let chars = |count: usize, tail: &str| format!("{}{tail}", "\u{e9}".repeat(count));
    assert_eq!(MONITORING_EVENTS_PER_RECORD_LIMIT_201, 32);
    assert_eq!(MONITORING_EVENT_EVIDENCE_LIMIT_201, 64 * 1024);
    assert_eq!(MONITORING_REPORT_OUTPUT_LIMIT_201, 1024 * 1024);

    for (field, limit) in [
        ("actual_value", 2500),
        ("tech_code", 50),
        ("tech_info", 500),
        ("transaction_id", 36),
    ] {
        let mut value = serde_json::to_value(event(1)).unwrap();
        value[field] = json!(chars(limit, ""));
        assert!(
            serde_json::from_value::<MonitoringEvent201>(value.clone()).is_ok(),
            "{field} at the limit counts characters, not bytes"
        );
        value[field] = json!(chars(limit, "x"));
        assert!(
            serde_json::from_value::<MonitoringEvent201>(value).is_err(),
            "{field} over the limit"
        );
    }

    let mut minimal = serde_json::to_value(event(1)).unwrap();
    for optional in [
        "cause",
        "tech_code",
        "tech_info",
        "cleared",
        "transaction_id",
        "variable_monitoring_id",
    ] {
        minimal.as_object_mut().unwrap().remove(optional);
    }
    minimal["actual_value"] = json!("");
    let decoded: MonitoringEvent201 = serde_json::from_value(minimal).unwrap();
    assert_eq!(decoded.cleared, None);
    assert_eq!(decoded.actual_value, "");

    let mut extra = serde_json::to_value(event(1)).unwrap();
    extra["severity"] = json!(3);
    assert!(
        serde_json::from_value::<MonitoringEvent201>(extra).is_err(),
        "severity is not on the wire and is never retained"
    );

    let mut full = serde_json::to_value(record(MonitoringEventOrdering201::InSequence)).unwrap();
    full["events"] = json!(vec![serde_json::to_value(event(1)).unwrap(); 32]);
    assert!(serde_json::from_value::<MonitoringEventRecord201>(full.clone()).is_ok());
    full["events"] = json!(vec![serde_json::to_value(event(1)).unwrap(); 33]);
    assert!(serde_json::from_value::<MonitoringEventRecord201>(full).is_err());

    let mut counters =
        serde_json::to_value(record(MonitoringEventOrdering201::InSequence)).unwrap();
    for counter in ["events_omitted", "shed_calls", "shed_events"] {
        counters.as_object_mut().unwrap().remove(counter);
    }
    let decoded: MonitoringEventRecord201 = serde_json::from_value(counters).unwrap();
    assert_eq!(
        (
            decoded.events_omitted,
            decoded.shed_calls,
            decoded.shed_events
        ),
        (0, 0, 0)
    );
    let quiet = MonitoringEventRecord201 {
        events_omitted: 0,
        shed_calls: 0,
        shed_events: 0,
        ..decoded
    };
    let encoded = serde_json::to_value(quiet).unwrap();
    assert!(encoded.get("shed_calls").is_none());
}
