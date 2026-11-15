use serde_json::json;
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};
use store::RecordingStore;
use uob_application::{
    ObservationCommitError, SecurityEventDecision16, SecurityEventGate16, SecurityEventKey16,
    SecurityEventObservation16, SecurityEventStore16, StationEvent, record_security_event_16,
    transaction16::TransactionContext,
};
use uob_contracts::*;

#[path = "security_event16/store.rs"]
mod store;

const SECRET: &str = "not-a-real-password-0000";

fn at(seconds: i64) -> UtcTimestamp {
    UtcTimestamp::new(
        time::OffsetDateTime::from_unix_timestamp(1_791_306_000 + seconds).expect("instant"),
    )
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

fn registration(snapshot: &mut StationSnapshot, status: &str) {
    snapshot.current_values.push(DataPointValue {
        point_id: PointId::new("ocpp16/registration/status").unwrap(),
        value: Some(TypedValue::Text(status.to_owned())),
        source_time: None,
        observed_at: at(0),
        quality: Quality {
            level: QualityLevel::Good,
            reason: None,
        },
        freshness: Freshness::Unknown,
        measurement: None,
    });
}

fn fixture(name: &str) -> StationSnapshot {
    let bytes = match name {
        "16" => include_bytes!("../../contracts/tests/fixtures/station-snapshot-ocpp16-v1.json")
            .as_slice(),
        _ => include_bytes!("../../contracts/tests/fixtures/station-snapshot-ocpp201-v1.json")
            .as_slice(),
    };
    serde_json::from_slice(bytes).unwrap()
}

/// The 1.6 fixture with current accepted registration.
fn station() -> StationSnapshot {
    let mut snapshot = fixture("16");
    registration(&mut snapshot, "Accepted");
    snapshot
}

fn context(sequence: u64) -> TransactionContext {
    TransactionContext {
        identity: serde_json::from_value(json!({
            "bridge_id":"bridge-berlin-1",
            "runtime":{"environment":"demo","release_id":"test","release_digest":"sha256:test",
                "process_instance_id":"test"}
        }))
        .unwrap(),
        event_id: EventId::new(format!("security-{sequence}")).unwrap(),
        sequence,
        correlation_id: Some(CorrelationId::new(format!("call-{sequence}")).unwrap()),
        target: None,
        delivery_deadline: at(86_400),
    }
}

fn observation() -> SecurityEventObservation16 {
    SecurityEventObservation16::new(
        "InvalidMessages",
        at(2),
        Some(&format!("bad frame, password={SECRET}")),
    )
    .unwrap()
}

#[test]
fn an_event_is_journaled_as_station_evidence_with_the_snapshot_activity() {
    let store = RecordingStore::default();
    let mut snapshot = station();
    let original = snapshot.clone();

    block_on(record_security_event_16(
        &store,
        &mut snapshot,
        observation(),
        3,
        context(5),
        at(10),
    ))
    .unwrap();

    {
        let writes = store.writes.lock().unwrap();
        assert_eq!(writes.len(), 1, "snapshot and journal share one write");
        assert_eq!(writes[0].station_snapshot.as_ref(), Some(&snapshot));
        assert!(writes[0].required_deliveries.is_empty());
    }
    assert_eq!(snapshot.observed_at, at(10));
    assert_eq!(
        snapshot.current_values, original.current_values,
        "no data point changes"
    );

    let journal = store.journal();
    assert_eq!(journal.len(), 1);
    let entry = &journal[0];
    assert_eq!(entry.event_type.as_str(), "station.security_event.16");
    assert_eq!(entry.origin, EventOrigin::Station);
    assert_eq!(entry.sequence, 5);
    assert_eq!(entry.source_time, Some(at(2)));
    assert_eq!(entry.observed_at, at(10));
    assert_eq!(entry.resource, snapshot.station);
    assert_eq!(entry.schema_version, snapshot.schema_version);
    assert_eq!(
        entry.payload,
        StationEvent::SecurityEvent16 {
            station_snapshot_invalidated: snapshot.station.station_id.clone(),
            security_event_16: SecurityEvent16 {
                event_type: "InvalidMessages".to_owned(),
                standard: true,
                timestamp: at(2),
                tech_info: Some("bad frame, password=[redacted]".to_owned()),
                tech_info_redactions: 1,
                shed_calls: 3,
            },
        }
    );
}

#[test]
fn the_journal_never_holds_the_raw_secret() {
    let store = RecordingStore::default();
    let mut snapshot = station();

    block_on(record_security_event_16(
        &store,
        &mut snapshot,
        observation(),
        0,
        context(5),
        at(10),
    ))
    .unwrap();

    let stored = serde_json::to_string(&store.journal()).unwrap();
    assert!(stored.contains("password=[redacted]"));
    assert!(!stored.contains(SECRET));
    assert!(!format!("{:?}", store.journal()).contains(SECRET));
}

#[test]
fn the_source_time_is_the_charger_timestamp_in_utc() {
    let store = RecordingStore::default();
    let mut snapshot = station();
    let local = time::OffsetDateTime::from_unix_timestamp(1_791_306_100)
        .unwrap()
        .to_offset(time::UtcOffset::from_hms(2, 0, 0).unwrap());
    let observation =
        SecurityEventObservation16::new("Acme.Vendor", UtcTimestamp::new(local), None).unwrap();

    block_on(record_security_event_16(
        &store,
        &mut snapshot,
        observation,
        0,
        context(5),
        at(200),
    ))
    .unwrap();

    let entry = &store.journal()[0];
    assert_eq!(entry.source_time, Some(at(100)));
    let encoded = serde_json::to_value(entry).unwrap();
    assert_eq!(encoded["payload"]["security_event_16"]["standard"], false);
    assert!(
        encoded["payload"]["security_event_16"]["timestamp"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
}

#[test]
fn a_failed_write_leaves_the_snapshot_unchanged_and_the_gate_keeps_its_shed_calls() {
    let store = RecordingStore::default();
    *store.fail.lock().unwrap() = true;
    let mut snapshot = station();
    let original = snapshot.clone();
    let mut gate = SecurityEventGate16::default();
    // 61 calls in one window: the last is shed and must be reported by the next stored record.
    for _ in 0..61 {
        gate.admit(at(0));
    }

    let SecurityEventDecision16::Store { shed_calls, .. } = gate.admit(at(60)) else {
        panic!("the window rolled over");
    };
    assert_eq!(shed_calls, 1);
    let error = block_on(record_security_event_16(
        &store,
        &mut snapshot,
        observation(),
        shed_calls,
        context(5),
        at(60),
    ))
    .unwrap_err();

    assert!(matches!(error, ObservationCommitError::Storage(_)));
    assert_eq!(snapshot, original);
    assert!(store.journal().is_empty());

    // The write failed, so the receipt was not committed: the retry reports the same loss.
    *store.fail.lock().unwrap() = false;
    let SecurityEventDecision16::Store {
        shed_calls,
        receipt,
    } = gate.admit(at(61))
    else {
        panic!("the retry is stored");
    };
    assert_eq!(shed_calls, 1);
    block_on(record_security_event_16(
        &store,
        &mut snapshot,
        observation(),
        shed_calls,
        context(5),
        at(61),
    ))
    .unwrap();
    gate.committed(receipt);

    assert_eq!(store.journal().len(), 1, "the retry commits exactly once");
    let SecurityEventDecision16::Store { shed_calls, .. } = gate.admit(at(62)) else {
        panic!("stored");
    };
    assert_eq!(shed_calls, 0);
}

#[test]
fn stations_without_current_accepted_16_registration_are_refused() {
    let store = RecordingStore::default();
    let mut unregistered = fixture("16");
    let mut pending = fixture("16");
    registration(&mut pending, "Pending");
    let mut rejected = fixture("16");
    registration(&mut rejected, "Rejected");
    let mut ocpp201 = fixture("201");
    registration(&mut ocpp201, "Accepted");
    ocpp201.current_values.last_mut().unwrap().point_id =
        PointId::new("ocpp201/registration/status").unwrap();

    for snapshot in [&mut unregistered, &mut pending, &mut rejected, &mut ocpp201] {
        let original = snapshot.clone();
        let error = block_on(record_security_event_16(
            &store,
            snapshot,
            observation(),
            0,
            context(5),
            at(10),
        ))
        .unwrap_err();
        assert!(matches!(error, ObservationCommitError::InvalidState));
        assert_eq!(*snapshot, original);
    }
    assert!(store.journal().is_empty());
}

#[test]
fn an_invalid_context_is_refused_before_any_write() {
    let store = RecordingStore::default();
    let mut snapshot = station();

    let zero_sequence = context(0);
    let mut foreign_bridge = context(5);
    foreign_bridge.identity.bridge_id = BridgeId::new("another-bridge").unwrap();

    for invalid in [zero_sequence, foreign_bridge] {
        let error = block_on(record_security_event_16(
            &store,
            &mut snapshot,
            observation(),
            0,
            invalid,
            at(10),
        ))
        .unwrap_err();
        assert!(matches!(error, ObservationCommitError::InvalidState));
    }
    assert!(store.writes.lock().unwrap().is_empty());
}

fn key(observation: &SecurityEventObservation16, snapshot: &StationSnapshot) -> SecurityEventKey16 {
    observation.key(snapshot.station.clone())
}

#[test]
fn a_redelivery_is_recognised_by_type_timestamp_and_redacted_text() {
    let store = RecordingStore::default();
    let mut snapshot = station();
    let first = observation();
    assert!(!block_on(store.security_event_recorded_16(key(&first, &snapshot))).unwrap());

    block_on(record_security_event_16(
        &store,
        &mut snapshot,
        first,
        0,
        context(5),
        at(10),
    ))
    .unwrap();

    // The same event, carrying a different raw secret that redacts to the same text.
    let redelivered = SecurityEventObservation16::new(
        "InvalidMessages",
        at(2),
        Some("bad frame, password=another-fake-secret-1111"),
    )
    .unwrap();
    assert!(block_on(store.security_event_recorded_16(key(&redelivered, &snapshot))).unwrap());

    let other_time = SecurityEventObservation16::new(
        "InvalidMessages",
        at(3),
        Some("bad frame, password=another-fake-secret-1111"),
    )
    .unwrap();
    let other_type = SecurityEventObservation16::new(
        "InvalidMessage",
        at(2),
        Some("bad frame, password=another-fake-secret-1111"),
    )
    .unwrap();
    let other_text = SecurityEventObservation16::new("InvalidMessages", at(2), None).unwrap();
    for different in [other_time, other_type, other_text] {
        assert!(!block_on(store.security_event_recorded_16(key(&different, &snapshot))).unwrap());
    }
}
