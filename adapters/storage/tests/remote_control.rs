use rusqlite::Connection;
use serde_json::Value;
use std::{path::PathBuf, time::Duration};
use uob_application::release_drain::ReleaseDrainPort;
use uob_application::remote_control::RemoteControlStore;
use uob_application::{AtomicStoreWrite, OperationalStore};
use uob_contracts::{Command, CommandLifecycle, CommandResult, RequestId, UtcTimestamp};
use uob_storage_adapter::SqliteOperationalStore;
type Store = SqliteOperationalStore<Value, String, String, String>;
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-remote-evidence-{}.db", uuid::Uuid::new_v4())))
    }
    fn open(&self) -> Store {
        Store::open(&self.0, 16).unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
fn id(value: &str) -> RequestId {
    RequestId::new(value).unwrap()
}
async fn admit(store: &Store, name: &str) -> Command<Value> {
    let mut command: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    command.request_id = id(name);
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command.clone());
    store.write_atomic(write).await.unwrap();
    command
}
#[tokio::test]
async fn allocation_is_atomic_recoverable_nonreused_and_bound_to_command_retention() {
    let database = Database::new();
    let store = database.open();
    assert!(store.reserve_remote_start(id("missing")).await.is_err());
    let command = admit(&store, "one").await;
    let (a, b) = tokio::join!(
        store.reserve_remote_start(id("one")),
        store.reserve_remote_start(id("one"))
    );
    assert_eq!(a.unwrap(), 1);
    assert_eq!(b.unwrap(), 1);
    store
        .record_remote_response(
            id("one"),
            "Accepted".to_owned(),
            Some("native-1".to_owned()),
        )
        .await
        .unwrap();
    assert!(
        store
            .record_remote_response(id("one"), "Rejected".to_owned(), None)
            .await
            .is_err()
    );
    assert!(
        store
            .record_remote_response(id("one"), "untrusted free text".to_owned(), None)
            .await
            .is_err()
    );
    let expected = store.remote_control_evidence(id("one")).await.unwrap();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let store = database.open();
    assert_eq!(
        store.remote_control_evidence(id("one")).await.unwrap(),
        expected
    );
    assert_eq!(store.reserve_remote_start(id("one")).await.unwrap(), 1);
    admit(&store, "two").await;
    assert_eq!(store.reserve_remote_start(id("two")).await.unwrap(), 2);
    let result = CommandResult {
        schema_version: command.schema_version,
        correlation_id: command.correlation_id.clone(),
        resource: command.resource.clone(),
        return_route: command.return_route(),
        recorded_at: command.admitted_at,
        lifecycle: CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None,
        },
        observed_effects: vec![],
    };
    let mut write = AtomicStoreWrite::empty();
    write.command_result = Some(result);
    store.write_atomic(write).await.unwrap();
    let later: UtcTimestamp = serde_json::from_str("\"2026-09-10T00:00:00Z\"").unwrap();
    store.prune_command_deduplication(later).await.unwrap();
    assert!(
        store
            .remote_control_evidence(id("one"))
            .await
            .unwrap()
            .is_none()
    );
    // The unresolved second command and its allocation survive pruning.
    assert_eq!(
        store
            .remote_control_evidence(id("two"))
            .await
            .unwrap()
            .unwrap()
            .remote_start_id,
        Some(2)
    );
    admit(&store, "three").await;
    assert_eq!(store.reserve_remote_start(id("three")).await.unwrap(), 3);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
#[tokio::test]
async fn exhaustion_and_release_drain_fail_closed_without_reusing_ids() {
    let database = Database::new();
    let store = database.open();
    admit(&store, "one").await;
    let window = store.begin_drain(Duration::from_secs(1)).await.unwrap();
    assert!(store.reserve_remote_start(id("one")).await.is_err());
    store.cancel_drain(window).await.unwrap();
    Connection::open(&database.0)
        .unwrap()
        .execute(
            "UPDATE remote_start_counter SET value = 2147483647 WHERE id = 1",
            [],
        )
        .unwrap();
    assert!(store.reserve_remote_start(id("one")).await.is_err());
    assert!(
        store
            .remote_control_evidence(id("one"))
            .await
            .unwrap()
            .is_none()
    );
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
