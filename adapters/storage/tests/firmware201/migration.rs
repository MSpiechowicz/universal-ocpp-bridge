use super::support::*;

#[tokio::test]
async fn schema_v18_database_gains_firmware201_jobs_without_losing_release_jobs() {
    let database = Database::new();
    drop(database.open());
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    connection
        .execute_batch(
            "DROP TABLE firmware201_jobs;
             INSERT INTO release_jobs(id, kind) VALUES ('firmware16/1', 'firmware');
             PRAGMA user_version = 18;",
        )
        .unwrap();
    drop(connection);
    let store = database.open();
    let command = admit(&store, "after-upgrade", 1, 1).await.unwrap();
    assert_eq!(state(&store, &command).await, FirmwareJobState201::Pending);
    assert_eq!(database.release_jobs().len(), 2);
    drop(store);
    let version: i64 = rusqlite::Connection::open(&database.0)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 20);
}

#[test]
fn newer_schema_is_refused() {
    let database = Database::new();
    drop(database.open());
    rusqlite::Connection::open(&database.0)
        .unwrap()
        .execute_batch("PRAGMA user_version = 21;")
        .unwrap();
    assert!(Store::open(&database.0, 32).is_err());
}
