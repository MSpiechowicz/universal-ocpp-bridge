use serde::Deserialize;
use serde_json::{Value, json};
use uob_contracts::*;

fn station() -> StationId {
    StationId::new("station-127").unwrap()
}

fn timestamp(value: &str) -> UtcTimestamp {
    serde_json::from_value(json!(value)).unwrap()
}

fn event() -> SecurityEvent16 {
    SecurityEvent16 {
        event_type: "InvalidFirmwareSignature".to_owned(),
        standard: true,
        timestamp: timestamp("2026-10-12T08:30:00Z"),
        tech_info: Some("signature [redacted]".to_owned()),
        tech_info_redactions: 1,
        shed_calls: 4,
    }
}

fn station_event(record: SecurityEvent16) -> StationEvent {
    StationEvent::SecurityEvent16 {
        station_snapshot_invalidated: station(),
        security_event_16: record,
    }
}

#[test]
fn security_events_round_trip_as_a_distinct_station_event() {
    let original = station_event(event());
    let encoded = serde_json::to_value(&original).unwrap();

    assert_eq!(encoded["station_snapshot_invalidated"], "station-127");
    assert_eq!(
        encoded["security_event_16"]["event_type"],
        "InvalidFirmwareSignature"
    );
    assert_eq!(encoded["security_event_16"]["standard"], true);
    assert_eq!(
        encoded["security_event_16"]["timestamp"],
        "2026-10-12T08:30:00Z"
    );
    assert_eq!(
        serde_json::from_value::<StationEvent>(encoded).unwrap(),
        original
    );
}

#[test]
fn security_events_never_decode_as_an_earlier_station_event() {
    let encoded = serde_json::to_value(station_event(event())).unwrap();

    match serde_json::from_value::<StationEvent>(encoded).unwrap() {
        StationEvent::SecurityEvent16 {
            security_event_16, ..
        } => assert_eq!(security_event_16, event()),
        other => panic!("security evidence decoded as {other:?}"),
    }

    let invalidation = StationEvent::Invalidation {
        station_snapshot_invalidated: station(),
    };
    assert_eq!(
        serde_json::from_value::<StationEvent>(serde_json::to_value(&invalidation).unwrap())
            .unwrap(),
        invalidation,
        "a plain marker never decodes as security evidence"
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
    MonitoringEvents201 {
        station_snapshot_invalidated: StationId,
        monitoring_events_201: MonitoringEventRecord201,
    },
    Invalidation {
        station_snapshot_invalidated: StationId,
    },
}

#[test]
fn released_readers_decode_security_evidence_as_an_invalidation_marker() {
    let encoded = serde_json::to_value(station_event(event())).unwrap();

    match serde_json::from_value::<ReleasedStationEvent>(encoded).unwrap() {
        ReleasedStationEvent::Invalidation {
            station_snapshot_invalidated,
        } => assert_eq!(station_snapshot_invalidated, station()),
        other => panic!("released reader misread security evidence: {other:?}"),
    }
}

#[test]
fn zero_counters_and_absent_tech_info_are_omitted() {
    let mut minimal = event();
    minimal.tech_info = None;
    minimal.tech_info_redactions = 0;
    minimal.shed_calls = 0;
    let encoded = serde_json::to_value(&minimal).unwrap();

    assert_eq!(
        encoded,
        json!({
            "event_type": "InvalidFirmwareSignature",
            "standard": true,
            "timestamp": "2026-10-12T08:30:00Z"
        })
    );
    assert_eq!(
        serde_json::from_value::<SecurityEvent16>(encoded).unwrap(),
        minimal
    );

    let full = serde_json::to_value(event()).unwrap();
    assert_eq!(full["tech_info"], "signature [redacted]");
    assert_eq!(full["tech_info_redactions"], 1);
    assert_eq!(full["shed_calls"], 4);
}

#[test]
fn fields_are_bounded_by_characters_and_unknown_fields_are_refused() {
    let chars = |count: usize, tail: &str| format!("{}{tail}", "\u{e9}".repeat(count));
    assert_eq!(SECURITY_EVENT_TYPE_MAX_CHARS_16, 50);
    assert_eq!(SECURITY_EVENT_TECH_INFO_MAX_CHARS_16, 1024);

    for (field, limit) in [("event_type", 50), ("tech_info", 1024)] {
        let mut value = serde_json::to_value(event()).unwrap();
        value[field] = json!(chars(limit, ""));
        assert!(
            serde_json::from_value::<SecurityEvent16>(value.clone()).is_ok(),
            "{field} at the limit counts characters, not bytes"
        );
        value[field] = json!(chars(limit, "x"));
        assert!(
            serde_json::from_value::<SecurityEvent16>(value).is_err(),
            "{field} over the limit"
        );
    }

    let mut extra = serde_json::to_value(event()).unwrap();
    extra["extra"] = json!(true);
    assert!(serde_json::from_value::<SecurityEvent16>(extra).is_err());

    let mut missing = serde_json::to_value(event()).unwrap();
    missing.as_object_mut().unwrap().remove("standard");
    assert!(serde_json::from_value::<SecurityEvent16>(missing).is_err());

    let mut local_offset = serde_json::to_value(event()).unwrap();
    local_offset["timestamp"] = json!("2026-10-12T10:30:00+02:00");
    let decoded: SecurityEvent16 = serde_json::from_value(local_offset).unwrap();
    assert_eq!(decoded.timestamp, timestamp("2026-10-12T08:30:00Z"));
}

#[test]
fn extended_trigger_classes_are_station_only_and_carry_revision_19() {
    assert_eq!(ContractVersion::V1_EXTENDED_TRIGGER_16.revision, 19);
    assert_eq!(ExportRecord::SCHEMA_VERSION.revision, 19);

    for (class, name) in [
        (
            TriggerMessageClass::LogStatusNotification,
            "LogStatusNotification",
        ),
        (
            TriggerMessageClass::SignedFirmwareStatusNotification,
            "SignedFirmwareStatusNotification",
        ),
    ] {
        assert_eq!(class.as_str(), name);
        assert_eq!(serde_json::to_value(class).unwrap(), json!(name));
        assert!(class.is_station_only());
        assert_eq!(
            class.contract_version(),
            ContractVersion::V1_EXTENDED_TRIGGER_16
        );
    }

    for class in [
        TriggerMessageClass::BootNotification,
        TriggerMessageClass::DiagnosticsStatusNotification,
        TriggerMessageClass::FirmwareStatusNotification,
        TriggerMessageClass::Heartbeat,
        TriggerMessageClass::MeterValues,
        TriggerMessageClass::StatusNotification,
    ] {
        assert_eq!(class.contract_version(), ContractVersion::V1_TRIGGER);
    }
    assert!(TriggerMessageClass::Heartbeat.is_station_only());
    assert!(!TriggerMessageClass::MeterValues.is_station_only());
    assert!(!TriggerMessageClass::StatusNotification.is_station_only());
}

#[test]
fn published_command_result_accepts_the_new_classes_and_older_revisions_do_not() {
    let schema = |source: &str| {
        let schema: Value = serde_json::from_str(source).unwrap();
        jsonschema::draft202012::new(&schema).unwrap()
    };
    let v18 = schema(include_str!("../schemas/v1.18/command-result.schema.json"));
    let v19 = schema(include_str!("../schemas/v1.19/command-result.schema.json"));
    let mut results: Value =
        serde_json::from_str(include_str!("fixtures/command-results-v1.json")).unwrap();
    let result = &mut results[1];

    for class in ["LogStatusNotification", "SignedFirmwareStatusNotification"] {
        result["schema_version"]["revision"] = json!(19);
        result["trigger_observation"] = json!({
            "requested_class": class,
            "native_scope": null,
            "expected_targets": [0],
            "dispatch_started_at": "2026-10-12T08:30:01Z",
            "deadline": "2026-10-12T08:31:01Z",
            "native_response": "Accepted",
            "status": "pending"
        });

        assert!(v19.is_valid(result), "{class}");
        assert!(!v18.is_valid(result), "{class} predates revision 19");
        let decoded: CommandResult = serde_json::from_value(result.clone()).unwrap();
        assert_eq!(
            decoded
                .trigger_observation
                .unwrap()
                .requested_class
                .as_str(),
            class
        );
    }
}
