use time::Duration;
use uob_application::{
    CommittedRecord, CommittedRecordId, StorageAdmissionState, StorageWritePurpose,
};
use uob_storage_adapter::SqliteRetentionPolicy;

use super::*;

#[test]
fn protected_reserve_refuses_new_start_but_accepts_active_session_completion() {
    let database = TestDatabase::new();
    let seed_store = Store::open(database.path(), 8).expect("open seed store");
    block_on(seed_store.write_atomic(delivery_write("event-seed", "delivery-seed")))
        .expect("seed retained critical work");
    let seed_bytes = block_on(seed_store.storage_retention_status())
        .expect("seed status")
        .used_bytes;
    drop(seed_store);

    let start_limit = seed_bytes + 64;
    let policy = SqliteRetentionPolicy::new(start_limit + 2048, 2048).expect("test policy");
    let store = Store::open_with_retention_policy(database.path(), 8, policy)
        .expect("reopen pressure store");

    let mut start = event_write("event-new-start", 2, "new session".repeat(64));
    start.purpose = StorageWritePurpose::NewSessionStart;
    let error = block_on(store.write_atomic(start)).expect_err("reserve rejects new session");
    assert_eq!(error.code(), StorageErrorCode::CapacityExhausted);

    let routine = event_write("event-routine", 2, "routine work".repeat(64));
    let error = block_on(store.write_atomic(routine)).expect_err("routine work preserves reserve");
    assert_eq!(error.code(), StorageErrorCode::CapacityExhausted);

    let completion = event_write("event-completion", 2, "session completion".repeat(32));
    let mut completion = completion;
    completion.purpose = StorageWritePurpose::ActiveSessionCompletion;
    block_on(store.write_atomic(completion)).expect("completion consumes protected reserve");
    let status = block_on(store.storage_retention_status()).expect("pressure status");
    assert_eq!(
        status.new_session_admission,
        StorageAdmissionState::CriticalCapacityExhausted
    );
    assert_eq!(status.retained_required_deliveries, 1);
}

#[test]
fn retention_keeps_outage_backlog_then_recovers_admission_after_final_delivery() {
    let database = TestDatabase::new();
    let seed_store = Store::open(database.path(), 8).expect("open seed store");
    block_on(seed_store.write_atomic(delivery_write("event-outage", "delivery-outage")))
        .expect("seed outage backlog");
    let seed_bytes = block_on(seed_store.storage_retention_status())
        .expect("seed status")
        .used_bytes;
    drop(seed_store);

    let policy = SqliteRetentionPolicy::new(seed_bytes + 1024, 1024).expect("test policy");
    let store = Store::open_with_retention_policy(database.path(), 8, policy)
        .expect("reopen retained store");
    let after_retention = UtcTimestamp::new(
        timestamp(0)
            .into_inner()
            .checked_add(Duration::days(8))
            .expect("later retention time"),
    );
    let retained = block_on(store.maintain_storage_retention(after_retention))
        .expect("maintain while delivery pending");
    assert_eq!(retained.pruned_expired_events, 0);
    assert_eq!(retained.retained_critical_events, 1);
    assert_eq!(retained.retained_required_deliveries, 1);
    assert_eq!(
        retained.new_session_admission,
        StorageAdmissionState::CriticalCapacityExhausted
    );

    block_on(store.record_delivery_attempt(DeliveryAttempt {
        report: DeliveryReport {
            delivery_id: text(DeliveryId::new, "delivery-outage"),
            outcome: DeliveryOutcome::Acknowledged {
                peer: "peer-a".to_owned(),
                scope: AcknowledgementScope("application-processing".to_owned()),
            },
            reported_at: timestamp(0),
        },
        resolution: DeliveryAttemptResolution::Final,
    }))
    .expect("finalize delivery");
    let recovered = block_on(store.maintain_storage_retention(after_retention))
        .expect("maintain after final delivery");
    assert_eq!(recovered.retained_critical_events, 0);
    assert_eq!(recovered.retained_required_deliveries, 0);
    assert_eq!(recovered.pruned_expired_events, 1);
    assert_eq!(recovered.pruned_expired_delivery_attempts, 1);
    assert_eq!(
        recovered.new_session_admission,
        StorageAdmissionState::Available
    );

    let mut next_start = event_write("event-after-recovery", 2, "next start".to_owned());
    next_start.purpose = StorageWritePurpose::NewSessionStart;
    block_on(store.write_atomic(next_start)).expect("admission recovers after safe pruning");
}

#[test]
fn pressure_sheds_best_effort_records_and_reports_each_category() {
    let database = TestDatabase::new();
    let store = Store::open_with_retention_policy(
        database.path(),
        8,
        SqliteRetentionPolicy::new(900, 100).expect("test policy"),
    )
    .expect("open bounded store");
    let mut write = delivery_write("event-critical", "delivery-critical");
    write.required_deliveries.push(PendingDelivery {
        durability: Durability::BestEffortTelemetry,
        delivery_id: text(DeliveryId::new, "delivery-telemetry"),
        event_id: text(EventId::new, "event-telemetry"),
        payload: "optional delivery".repeat(64),
        ..message_delivery("event-telemetry", "delivery-placeholder", resource())
    });
    write.committed_records.push(CommittedRecord {
        record_id: text(CommittedRecordId::new, "record-telemetry"),
        durability: Durability::BestEffortTelemetry,
        committed_at: timestamp(0),
        record: "optional telemetry".repeat(64),
    });

    block_on(store.write_atomic(write)).expect("critical data survives pressure");
    let status = block_on(store.storage_retention_status()).expect("retention status");
    assert_eq!(status.retained_critical_events, 1);
    assert_eq!(status.retained_required_deliveries, 1);
    assert_eq!(status.dropped_best_effort_deliveries, 1);
    assert_eq!(status.dropped_best_effort_telemetry, 1);
}

fn event_write(
    event_id: &str,
    sequence: u64,
    payload: String,
) -> AtomicStoreWrite<String, String, String, String> {
    let mut event = event_for(event_id, resource(), sequence);
    event.payload = payload;
    AtomicStoreWrite {
        purpose: StorageWritePurpose::Routine,
        station_snapshot: None,
        authorization_changes: Vec::new(),
        command: None,
        command_result: None,
        journal_events: vec![event],
        required_deliveries: Vec::new(),
        committed_records: Vec::new(),
    }
}
