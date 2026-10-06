use super::support::*;
use rusqlite::Connection;
use serde_json::{Value, json};

struct Legacy {
    snapshot: StationSnapshot,
    command: Command<Value>,
    result: CommandResult,
    event: EventEnvelope<String>,
    policy: AuthorizationChange,
    delivery: PendingDelivery<String>,
}
fn legacy() -> Legacy {
    let snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .unwrap();
    assert!(
        !snapshot.transactions.is_empty(),
        "migration must preserve real transaction evidence"
    );
    let command: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    let result = result(&command, CommandLifecycle::Admitted, 0);
    let mut raw: Value = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/event-envelope-v1.json"
    ))
    .unwrap();
    raw["resource"] = serde_json::to_value(&snapshot.station).unwrap();
    raw["payload"] = json!("retained-v14-journal");
    let event: EventEnvelope<String> = serde_json::from_value(raw).unwrap();
    let policy = AuthorizationChange {
        reference: AuthorizationReference::new("opaque-v14-policy".to_owned()).unwrap(),
        resource: snapshot.station.clone(),
        state: AuthorizationState::Active,
        revision: 7,
        changed_at: snapshot.observed_at,
        expires_at: None,
    };
    let delivery = PendingDelivery {
        delivery_id: DeliveryId::new("v14-pending".to_owned()).unwrap(),
        event_id: event.event_id.clone(),
        target_instance_id: TargetInstanceId::new("v14-target").unwrap(),
        target_configuration_revision: 3,
        ordering_key: snapshot.station.clone(),
        deadline: command.expires_at,
        durability: Durability::Critical,
        payload: "retained-v14-outbox".to_owned(),
    };
    Legacy {
        snapshot,
        command,
        result,
        event,
        policy,
        delivery,
    }
}
async fn seed_v14(database: &Database) -> (Legacy, CommittedRecordCursor) {
    let data = legacy();
    let store = database.open();
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(data.snapshot.clone());
    write.command = Some(data.command.clone());
    write.command_result = Some(data.result.clone());
    write.journal_events.push(data.event.clone());
    write.authorization_changes.push(data.policy.clone());
    write.required_deliveries.push(data.delivery.clone());
    write.committed_records.push(CommittedRecord {
        record_id: CommittedRecordId::new("v14-export-record".to_owned()).unwrap(),
        durability: Durability::Critical,
        committed_at: data.snapshot.observed_at,
        record: "retained-v14-export-checkpoint".to_owned(),
    });
    store.write_atomic(write).await.unwrap();
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap();
    let checkpoint = store
        .read_committed_records(
            CommittedRecordQuery {
                durability: Durability::Critical,
                after: None,
                limit: PageLimit::new(8).unwrap(),
            },
            &budget,
        )
        .await
        .unwrap()
        .resume_cursor;
    drop(store);
    // The v15 operational schema is exactly v14 plus this additive reservation table.
    Connection::open(&database.0)
        .unwrap()
        .execute_batch("DROP TABLE reservations16; PRAGMA user_version=14;")
        .unwrap();
    (data, checkpoint)
}
#[tokio::test]
async fn populated_v14_upgrade_preserves_policy_dedup_transactions_journal_outbox_and_export_resume()
 {
    let database = Database::new();
    let (data, checkpoint) = seed_v14(&database).await;
    let store = database.open();
    assert_eq!(
        store
            .station_snapshot(data.snapshot.station.clone())
            .await
            .unwrap(),
        Some(data.snapshot.clone())
    );
    assert_eq!(
        store
            .command_by_request_id(data.command.request_id.clone())
            .await
            .unwrap(),
        Some(data.command.clone())
    );
    assert_eq!(
        store
            .command_result_by_request_id(data.command.request_id.clone())
            .await
            .unwrap(),
        Some(data.result.clone())
    );
    let recovery = store
        .recover(RecoveryQuery {
            after_command: None,
            limit: PageLimit::new(16).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(recovery.authorization, vec![data.policy]);
    assert_eq!(recovery.pending_deliveries, vec![data.delivery]);
    let events = store
        .read_retained_events(RetainedEventQuery {
            resource: data.snapshot.station.clone(),
            after: None,
            limit: PageLimit::new(16).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(events.events, vec![data.event.clone()]);
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap();
    let resumed = store
        .read_committed_records(
            CommittedRecordQuery {
                durability: Durability::Critical,
                after: Some(checkpoint.clone()),
                limit: PageLimit::new(8).unwrap(),
            },
            &budget,
        )
        .await
        .unwrap();
    assert!(resumed.items.is_empty());
    assert_eq!(resumed.resume_cursor, checkpoint);
    let mut duplicate = AtomicStoreWrite::empty();
    duplicate.command = Some(data.command);
    duplicate.journal_events.push(data.event);
    assert!(matches!(
        store.write_atomic(duplicate).await.unwrap().command,
        Some(CommandAdmissionOutcome::Duplicate { .. })
    ));
    assert!(
        store
            .reservations_16(data.snapshot.station)
            .await
            .unwrap()
            .is_empty()
    );
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        16
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM committed_records", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[tokio::test]
async fn migration_failure_keeps_populated_v14_and_rolls_back_the_additive_table() {
    let database = Database::new();
    let (data, _) = seed_v14(&database).await;
    let connection = Connection::open(&database.0).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_migration BEFORE UPDATE ON event_sequence_counter BEGIN SELECT RAISE(ABORT,'injected migration failure'); END;").unwrap();
    drop(connection);
    assert!(Store::open(&database.0, 32).is_err());
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        14
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='reservations16'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let payload: String = connection
        .query_row("SELECT payload FROM station_snapshots", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<StationSnapshot>(&payload).unwrap(),
        data.snapshot
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM commands", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM authorization_changes", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM journal_events", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM target_deliveries", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM committed_records", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[tokio::test]
async fn reservation_admission_and_companion_state_roll_back_when_the_final_journal_write_fails() {
    let database = Database::new();
    let store = database.open();
    let connection = Connection::open(&database.0).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_journal BEFORE INSERT ON journal_events BEGIN SELECT RAISE(ABORT,'injected journal failure'); END;").unwrap();
    let candidate = candidate(1, 1);
    let command = command("atomic-reservation", -1, Some(&candidate), 1);
    let data = legacy();
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command.clone());
    write.command_result = Some(result(&command, CommandLifecycle::Admitted, 1));
    write.station_snapshot = Some(data.snapshot.clone());
    write.authorization_changes.push(data.policy);
    write.reservation_16 = Some(Box::new(ReservationMutation16 {
        station: station(),
        request_id: command.request_id.clone(),
        reservation_id: -1,
        admitted_at: at(1),
        generation: 1,
        mutation: ReservationMutationKind16::Reserve(candidate),
    }));
    write.journal_events.push(data.event);
    assert!(store.write_atomic(write).await.is_err());
    assert!(
        store
            .command_by_request_id(command.request_id.clone())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .command_result_by_request_id(command.request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(records(&store).await.is_empty());
    assert!(
        store
            .station_snapshot(data.snapshot.station)
            .await
            .unwrap()
            .is_none()
    );
    for table in ["authorization_changes", "journal_events", "command_results"] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
