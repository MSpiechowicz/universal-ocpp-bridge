//! The production dedup query is an exact-key index search whose cost does not depend on how many
//! other events share the station and timestamp.
use super::{Query, RECORDED, run};
use crate::SqliteOperationalStore;
use rusqlite::{Connection, StatementStatus, params};
use serde_json::json;
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

const FLOODED: i64 = 10_000;
const TIMESTAMP: &str = "1970-01-01T00:00:00Z";

struct Database(PathBuf);

impl Database {
    async fn migrated() -> Self {
        let path = std::env::temp_dir().join(format!("uob-security16-plan-{}.db", Uuid::new_v4()));
        let store = SqliteOperationalStore::<(), (), (), ()>::open(&path, 2).unwrap();
        store.shutdown(Duration::from_secs(1)).await.unwrap();

        Self(path)
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

fn query(event_type: &str, tech_info: Option<&str>) -> Query {
    Query {
        station: "station".to_owned(),
        timestamp: TIMESTAMP.to_owned(),
        event_type: event_type.to_owned(),
        tech_info: tech_info.map(str::to_owned),
    }
}

/// The envelope shape the journal serializes for a security event.
fn insert(connection: &Connection, station: &str, number: i64, tech_info: Option<&str>) {
    let mut event = json!({
        "event_type": "InvalidMessages", "standard": true, "timestamp": TIMESTAMP
    });
    if let Some(text) = tech_info {
        event["tech_info"] = json!(text);
    }
    let payload = json!({"payload": {"security_event_16": event}}).to_string();

    connection
        .execute(
            "INSERT INTO journal_events(event_id, resource, sequence, payload, retain_until)
             VALUES (?1, ?2, ?3, ?4, NULL)",
            params![format!("{station}-{number}"), station, number, payload],
        )
        .unwrap();
}

fn flood(connection: &Connection) {
    connection.execute_batch("BEGIN").unwrap();
    for number in 0..FLOODED {
        insert(
            connection,
            "station",
            number,
            Some(&format!("detail {number}")),
        );
    }
    connection.execute_batch("COMMIT").unwrap();
}

#[tokio::test]
async fn the_query_searches_the_index_on_all_four_key_parts() {
    let database = Database::migrated().await;
    let connection = Connection::open(&database.0).unwrap();
    let mut statement = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {RECORDED}"))
        .unwrap();
    let plan = statement
        .query_map(["station", TIMESTAMP, "InvalidMessages", "x"], |row| {
            row.get::<_, String>(3)
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");

    let search = plan
        .lines()
        .find(|line| line.contains("SEARCH"))
        .unwrap_or_else(|| panic!("no index search in plan:\n{plan}"));
    assert!(
        search.contains("USING INDEX security16_station_events"),
        "{plan}"
    );
    // The outer `EXISTS` select has no table; only that constant row is "scanned".
    let scans = plan.lines().filter(|line| line.contains("SCAN"));
    assert!(scans.eq(["SCAN CONSTANT ROW"]), "{plan}");
    // Resource, timestamp, event type and techInfo: four `=` or `IS` constraints.
    let constrained = search.matches("=?").count() + search.matches("IS ?").count();
    assert_eq!(constrained, 4, "{plan}");
}

#[tokio::test]
async fn the_lookup_examines_one_row_among_ten_thousand_with_the_same_timestamp() {
    let database = Database::migrated().await;
    let connection = Connection::open(&database.0).unwrap();
    flood(&connection);

    let mut statement = connection.prepare(RECORDED).unwrap();
    let mut steps = |query: Query| {
        statement.reset_status(StatementStatus::VmStep);
        let answer = run(&mut statement, &query).unwrap();

        (answer, statement.get_status(StatementStatus::VmStep))
    };

    let (hit, hit_steps) = steps(query("InvalidMessages", Some("detail 7777")));
    let (miss, miss_steps) = steps(query("InvalidMessages", Some("detail 70000")));
    let (absent, absent_steps) = steps(query("InvalidMessages", None));
    let (other_type, _) = steps(query("MemoryExhaustion", Some("detail 7777")));

    assert!(hit && !miss && !absent && !other_type);
    // A scan of the flooded rows would step at least once per row.
    for taken in [hit_steps, miss_steps, absent_steps] {
        assert!(taken < 100, "{taken} VM steps for {FLOODED} rows");
    }
    assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
}
