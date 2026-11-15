use serde_json::json;
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};
use store::RecordingStore;
use uob_application::{
    MonitoringEventDecision201, MonitoringEventGate201, NotifyEventObservation201,
    ObservationCommitError, StationEvent, record_monitoring_events_201,
    transaction16::TransactionContext,
};
use uob_contracts::*;

#[path = "monitoring201/store.rs"]
mod store;

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

fn accepted_registration(snapshot: &mut StationSnapshot, namespace: &str) {
    snapshot.current_values.push(DataPointValue {
        point_id: PointId::new(format!("{namespace}/registration/status")).unwrap(),
        value: Some(TypedValue::Text("Accepted".to_owned())),
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

/// The 2.0.1 fixture with current accepted registration.
fn station() -> StationSnapshot {
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
    ))
    .unwrap();
    accepted_registration(&mut snapshot, "ocpp201");
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
        event_id: EventId::new(format!("monitoring-{sequence}")).unwrap(),
        sequence,
        correlation_id: Some(CorrelationId::new(format!("call-{sequence}")).unwrap()),
        target: None,
        delivery_deadline: at(86_400),
    }
}

fn event() -> MonitoringEvent201 {
    MonitoringEvent201 {
        event_id: 1,
        timestamp: at(1),
        trigger: EventTrigger201::Alerting,
        cause: None,
        actual_value: "7500.5".to_owned(),
        tech_code: None,
        tech_info: None,
        cleared: None,
        transaction_id: None,
        variable_monitoring_id: Some(7),
        notification: EventNotification201::CustomMonitor,
        component: DeviceComponent201 {
            name: "EVSE".to_owned(),
            instance: None,
            evse: None,
        },
        variable: DeviceVariable201 {
            name: "Power".to_owned(),
            instance: None,
        },
    }
}

fn record() -> MonitoringEventRecord201 {
    let observation = NotifyEventObservation201 {
        generated_at: at(2),
        seq_no: 0,
        tbc: false,
        events: vec![event()],
    };
    match MonitoringEventGate201::default().admit(observation, at(3)) {
        MonitoringEventDecision201::Record { record, .. } => record,
        MonitoringEventDecision201::Shed { .. } => panic!("an empty gate journals the first call"),
    }
}

#[test]
fn a_record_is_journaled_as_station_evidence_with_the_snapshot_activity() {
    let store = RecordingStore::default();
    let mut snapshot = station();
    let original = snapshot.clone();
    let record = record();

    block_on(record_monitoring_events_201(
        &store,
        &mut snapshot,
        record.clone(),
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
    assert_eq!(entry.event_type.as_str(), "station.monitoring_event.201");
    assert_eq!(entry.origin, EventOrigin::Station);
    assert_eq!(entry.sequence, 5);
    assert_eq!(entry.source_time, Some(record.generated_at));
    assert_eq!(entry.observed_at, at(10));
    assert_eq!(entry.resource, snapshot.station);
    assert_eq!(
        entry.payload,
        StationEvent::MonitoringEvents201 {
            station_snapshot_invalidated: snapshot.station.station_id.clone(),
            monitoring_events_201: record,
        }
    );
}

#[test]
fn a_failed_write_leaves_the_snapshot_unchanged() {
    let store = RecordingStore::default();
    *store.fail.lock().unwrap() = true;
    let mut snapshot = station();
    let original = snapshot.clone();

    let error = block_on(record_monitoring_events_201(
        &store,
        &mut snapshot,
        record(),
        context(5),
        at(10),
    ))
    .unwrap_err();

    assert!(matches!(error, ObservationCommitError::Storage(_)));
    assert_eq!(snapshot, original);
    assert!(store.journal().is_empty());
}

#[test]
fn stations_without_current_accepted_201_registration_are_refused() {
    let store = RecordingStore::default();
    let mut unregistered: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
    ))
    .unwrap();
    let mut ocpp16: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .unwrap();
    accepted_registration(&mut ocpp16, "ocpp16");

    for snapshot in [&mut unregistered, &mut ocpp16] {
        let original = snapshot.clone();
        let error = block_on(record_monitoring_events_201(
            &store,
            snapshot,
            record(),
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
        let error = block_on(record_monitoring_events_201(
            &store,
            &mut snapshot,
            record(),
            invalid,
            at(10),
        ))
        .unwrap_err();
        assert!(matches!(error, ObservationCommitError::InvalidState));
    }
    assert!(store.writes.lock().unwrap().is_empty());
}
