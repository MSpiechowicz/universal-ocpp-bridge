use super::support::*;
use rusqlite::Connection;

fn table_count(connection: &Connection) -> i64 {
    connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='reservations201'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}
fn version(connection: &Connection) -> i64 {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}
/// Produces a v15 file: the current schema minus the additive 2.0.1 table, holding one
/// settled 1.6 reservation row in its exact persisted codec shape.
fn seed_v15(database: &Database) {
    drop(database.open());
    // The station key is the canonical station-only resource identity.
    let station = serde_json::to_string(&station()).unwrap();
    let key = serde_json::to_string(&[1_u8; 32]).unwrap();
    let payload = format!(
        r#"{{"station":{station},"request_id":"legacy","reservation_id":3,"revision":1,"candidate":{{"connector_id":1,"expiry_date":"1970-01-01T00:16:40Z","token_key":{key},"group_key":null}},"state":"active","admitted_at":"1970-01-01T00:00:01Z","changed_at":"1970-01-01T00:00:03Z","source_time":null,"source_observed_at":null,"started":true,"unresolved":false,"ambiguous":false,"history_floor":null}}"#
    );
    let connection = Connection::open(&database.0).unwrap();
    connection
        .execute(
            "INSERT INTO reservations16(station,revision,request_id,reservation_id,inflight,payload)
             VALUES(?1,1,'legacy',3,0,?2)",
            [&station, &payload],
        )
        .unwrap();
    connection
        .execute_batch("DROP TABLE reservations201; PRAGMA user_version=15;")
        .unwrap();
}

#[tokio::test]
async fn populated_v15_upgrade_adds_the_201_owner_without_touching_16_rows() {
    let database = Database::new();
    seed_v15(&database);
    let store = database.open();
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(version(&connection), 19);
    assert_eq!(table_count(&connection), 1);
    drop(connection);
    let legacy = store.reservations_16(station()).await.unwrap();
    assert_eq!(legacy.len(), 1, "the 1.6 owner table is preserved verbatim");
    assert_eq!(legacy[0].state, ReservationState16::Active);
    assert!(records(&store).await.is_empty());
    let command = reserve(&store, "after-upgrade", 3, Some(candidate(2, 2)), 10)
        .await
        .unwrap();
    dispatched(&store, &command, 11).await;
    response(&store, &command, "Accepted", 12).await;
    assert_eq!(records(&store).await[0].state, ReservationState201::Active);
    assert_eq!(
        store.reservations_16(station()).await.unwrap()[0].state,
        ReservationState16::Active,
        "the same reservationId in the other edition is a different owner"
    );
}

#[tokio::test]
async fn failed_migration_keeps_v15_and_rolls_back_the_additive_table() {
    let database = Database::new();
    seed_v15(&database);
    let connection = Connection::open(&database.0).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_migration BEFORE UPDATE ON event_sequence_counter BEGIN SELECT RAISE(ABORT,'injected migration failure'); END;").unwrap();
    drop(connection);
    assert!(Store::open(&database.0, 32).is_err());
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(version(&connection), 15);
    assert_eq!(table_count(&connection), 0);
}
