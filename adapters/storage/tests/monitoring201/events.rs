use super::support::*;
use std::time::Duration;
use uob_application::{AtomicStoreWrite, OperationalStore, PageLimit, RetainedEventQuery};
use uob_contracts::*;

fn resource() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge-a").unwrap(),
        station_id: StationId::new("station-126").unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}

fn monitoring_event(id: i32) -> MonitoringEvent201 {
    MonitoringEvent201 {
        event_id: id,
        timestamp: at("2026-10-08T10:15:00Z"),
        trigger: EventTrigger201::Alerting,
        cause: Some(id - 1),
        actual_value: "7500.5".to_owned(),
        tech_code: Some("E42".to_owned()),
        tech_info: None,
        cleared: Some(false),
        transaction_id: Some("tx-126".to_owned()),
        variable_monitoring_id: Some(7),
        notification: EventNotification201::CustomMonitor,
        component: component(),
        variable: variable(),
    }
}

fn envelope() -> EventEnvelope<StationEvent> {
    EventEnvelope {
        event_id: EventId::new("bridge-a/event/10").unwrap(),
        schema_version: ContractVersion::V1_MONITORING_201,
        runtime: serde_json::from_value(serde_json::json!({
            "environment":"demo", "release_id":"test", "release_digest":"sha256:test",
            "process_instance_id":"test-process"
        }))
        .unwrap(),
        resource: resource(),
        source_time: Some(at("2026-10-08T10:15:01Z")),
        observed_at: at("2026-10-08T10:15:02Z"),
        event_type: EventType::new("station.monitoring_event.201").unwrap(),
        origin: EventOrigin::Station,
        sequence: 10,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StationEvent::MonitoringEvents201 {
            station_snapshot_invalidated: resource().station_id,
            monitoring_events_201: MonitoringEventRecord201 {
                generated_at: at("2026-10-08T10:15:01Z"),
                seq_no: 4,
                tbc: true,
                ordering: MonitoringEventOrdering201::Gap { expected: 3 },
                events: vec![monitoring_event(2), monitoring_event(3)],
                events_omitted: 5,
                shed_calls: 2,
                shed_events: 9,
            },
        },
    }
}

async fn retained(
    store: &Store,
) -> Result<Vec<EventEnvelope<StationEvent>>, uob_application::StorageError> {
    store
        .read_retained_events(RetainedEventQuery {
            resource: resource(),
            after: None,
            limit: PageLimit::new(10).unwrap(),
        })
        .await
        .map(|page| page.events)
}

#[tokio::test]
async fn monitoring_events_round_trip_through_the_journal_and_a_reopen() {
    let database = Database::new();
    let store = database.open();
    let mut write = AtomicStoreWrite::empty();
    write.journal_events.push(envelope());
    store.write_atomic(write).await.unwrap();

    assert_eq!(retained(&store).await.unwrap(), vec![envelope()]);

    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    assert_eq!(retained(&reopened).await.unwrap(), vec![envelope()]);
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn a_stored_record_beyond_its_contract_bounds_is_never_served_as_monitoring_evidence() {
    let database = Database::new();
    let store = database.open();
    let mut write = AtomicStoreWrite::empty();
    write.journal_events.push(envelope());
    store.write_atomic(write).await.unwrap();

    let mut payload = serde_json::to_value(envelope()).unwrap();
    payload["payload"]["monitoring_events_201"]["events"] = serde_json::Value::Array(
        (0..=MONITORING_EVENTS_PER_RECORD_LIMIT_201)
            .map(|id| serde_json::to_value(monitoring_event(i32::try_from(id).unwrap())).unwrap())
            .collect(),
    );
    database
        .connection()
        .execute(
            "UPDATE journal_events SET payload = ?1",
            [payload.to_string()],
        )
        .unwrap();

    // Like every other untagged station event, an invalid record degrades to the plain marker.
    let events = retained(&store).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].payload,
        StationEvent::Invalidation {
            station_snapshot_invalidated: resource().station_id
        }
    );
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
