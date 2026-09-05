use super::*;
use std::time::Duration;

#[tokio::test]
async fn shutdown_drains_accepted_writes_rejects_clones_and_retains_recovery_records() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).unwrap();
    let clone = store.clone();
    // Admission happens before the future is polled. Closing must still drain this work.
    let accepted = store.write_atomic(write_with(
        vec![event("event-drain")],
        vec![delivery("event-drain", 4, "delivery-drain")],
    ));
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    accepted.await.unwrap();
    assert_eq!(
        clone
            .write_atomic(write_with(Vec::new(), Vec::new()))
            .await
            .unwrap_err()
            .code(),
        StorageErrorCode::Unavailable
    );
    let reopened = Store::open(database.path(), 8).unwrap();
    let deliveries = ready_deliveries(&reopened, timestamp(1));
    assert_eq!(deliveries.len(), 1);
    assert_eq!(
        deliveries[0].delivery.delivery_id.as_str(),
        "delivery-drain"
    );
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn locked_sqlite_misses_deadline_without_corrupting_committed_outbox() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).unwrap();
    store
        .write_atomic(write_with(
            vec![event("event-before-stop")],
            vec![delivery("event-before-stop", 4, "delivery-before-stop")],
        ))
        .await
        .unwrap();
    let lock = Connection::open(database.path()).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let accepted = store.write_atomic(write_with(
        vec![event_for("event-during-stop", resource(), 2)],
        vec![delivery("event-during-stop", 4, "delivery-during-stop")],
    ));
    assert!(store.shutdown(Duration::from_millis(10)).await.is_err());
    let committed: i64 = lock
        .query_row("SELECT count(*) FROM target_deliveries", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        committed, 1,
        "previous committed delivery survives the timeout"
    );
    lock.execute_batch("ROLLBACK").unwrap();
    store.shutdown(Duration::from_secs(2)).await.unwrap();
    accepted.await.unwrap();
    let reopened = Store::open(database.path(), 8).unwrap();
    let committed: i64 = lock
        .query_row("SELECT count(*) FROM target_deliveries", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(committed, 2);
    assert!(!ready_deliveries(&reopened, timestamp(1)).is_empty());
    let integrity: String = lock
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}
