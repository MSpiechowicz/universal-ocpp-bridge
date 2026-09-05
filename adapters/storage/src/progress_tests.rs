use super::SqliteOperationalStore;
use crate::{lifecycle::Worker, worker};
use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

#[tokio::test]
async fn probe_uses_operational_worker_and_never_reports_a_stalled_queue_as_progress() {
    let path = std::env::temp_dir().join(format!(
        "uob-probe-{}-{}.sqlite3",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut store = SqliteOperationalStore::<(), (), (), ()>::open(&path, 2).unwrap();
    store.probe_progress().await.unwrap();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    assert!(store.probe_progress().await.is_err());

    // Gate the real worker before it consumes its queue; no production fault hook.
    let (sender, receiver) = mpsc::sync_channel(2);
    let (release, stalled) = mpsc::channel();
    let connection = rusqlite::Connection::open(&path).unwrap();
    let policy = store.retention_policy;
    let join = std::thread::spawn(move || {
        stalled.recv().unwrap();
        worker::run(connection, receiver, policy);
    });
    store.sender = Arc::new(Worker::new(sender, join));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), store.probe_progress())
            .await
            .is_err()
    );
    release.send(()).unwrap();
    store.probe_progress().await.unwrap();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    std::fs::remove_file(path).unwrap();
}
