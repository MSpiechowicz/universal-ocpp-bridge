use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use uob_application::{
    AtomicStoreWrite, OperationalStore, StorageAdmissionState, StorageError, StorageErrorCode,
    release_drain::{ReleaseDrainPort, ReleaseJobKind},
};
use uob_release_manager::drain::{StagingStopPort, wait_for_idle_boundary};
use uob_storage_adapter::SqliteOperationalStore;
type Store = SqliteOperationalStore<(), (), (), ()>;
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        Self(std::env::temp_dir().join(format!(
            "uob-drain-{}-{}.sqlite",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
    fn open(&self) -> Arc<Store> {
        Arc::new(Store::open(&self.0, 16).unwrap())
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _removed = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
#[derive(Clone, Copy)]
enum Behavior {
    Stop,
    Fail,
    Hang,
    LateJob,
}
struct Staging {
    store: Arc<Store>,
    stopped: AtomicBool,
    behavior: Behavior,
}
impl Staging {
    fn new(store: Arc<Store>, behavior: Behavior) -> Self {
        Self {
            store,
            stopped: AtomicBool::new(false),
            behavior,
        }
    }
}
impl StagingStopPort for Staging {
    fn stop_and_confirm(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send + '_>> {
        Box::pin(async move {
            // Proves the supervisor has drained before asking staging to stop.
            assert_eq!(
                self.store
                    .storage_retention_status()
                    .await?
                    .new_session_admission,
                StorageAdmissionState::ReleaseDraining
            );
            match self.behavior {
                Behavior::Fail => {
                    return Err(StorageError::new(
                        StorageErrorCode::Unavailable,
                        "staging stop failed",
                    ));
                }
                Behavior::Hang => std::future::pending::<()>().await,
                Behavior::LateJob => {
                    self.store
                        .start_release_job("late".into(), ReleaseJobKind::Certificate)
                        .await?;
                }
                Behavior::Stop => {}
            }
            self.stopped.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}
fn run(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(future);
}

#[test]
fn staging_stops_before_boundary_and_drop_restores_writes() {
    run(async {
        let database = Database::new();
        let store = database.open();
        let staging = Staging::new(store.clone(), Behavior::Stop);
        let boundary = wait_for_idle_boundary(store.clone(), &staging, Duration::from_secs(2))
            .await
            .unwrap();
        assert!(staging.stopped.load(Ordering::SeqCst));
        boundary.validate().await.unwrap();
        assert!(store.write_atomic(AtomicStoreWrite::empty()).await.is_err());
        assert!(
            boundary.validate().await.is_err(),
            "late attempted write invalidates the original boundary"
        );
        drop(boundary);
        store.write_atomic(AtomicStoreWrite::empty()).await.unwrap();
        assert_eq!(
            store
                .storage_retention_status()
                .await
                .unwrap()
                .new_session_admission,
            StorageAdmissionState::Available
        );
        store.shutdown(Duration::from_secs(1)).await.unwrap();
    });
}

#[test]
fn late_job_after_staging_stop_prevents_boundary_and_survives_deferral() {
    run(async {
        let database = Database::new();
        let store = database.open();
        let staging = Staging::new(store.clone(), Behavior::LateJob);
        assert!(
            wait_for_idle_boundary(store.clone(), &staging, Duration::from_secs(2))
                .await
                .is_err()
        );
        assert!(staging.stopped.load(Ordering::SeqCst));
        let id = store.begin_drain(Duration::from_secs(1)).await.unwrap();
        assert_eq!(
            store.observe_drain(id.clone()).await.unwrap().stateful_jobs,
            1
        );
        store.cancel_drain(id).await.unwrap();
        store.shutdown(Duration::from_secs(1)).await.unwrap();
    });
}

#[test]
fn deadline_does_not_force_jobs_to_finish_or_stop_staging_early() {
    run(async {
        let database = Database::new();
        let store = database.open();
        store
            .start_release_job("firmware".into(), ReleaseJobKind::Firmware)
            .await
            .unwrap();
        let staging = Staging::new(store.clone(), Behavior::Stop);
        assert!(
            wait_for_idle_boundary(store.clone(), &staging, Duration::from_millis(40))
                .await
                .is_err()
        );
        assert!(!staging.stopped.load(Ordering::SeqCst));
        assert_eq!(
            store
                .storage_retention_status()
                .await
                .unwrap()
                .new_session_admission,
            StorageAdmissionState::Available
        );
        let id = store.begin_drain(Duration::from_secs(1)).await.unwrap();
        assert_eq!(
            store.observe_drain(id.clone()).await.unwrap().stateful_jobs,
            1
        );
        store.cancel_drain(id).await.unwrap();
        store.shutdown(Duration::from_secs(1)).await.unwrap();
    });
}

#[test]
fn confirmed_job_completion_allows_waiting_promotion_to_reach_idle() {
    run(async {
        let database = Database::new();
        let store = database.open();
        store
            .start_release_job("firmware".into(), ReleaseJobKind::Firmware)
            .await
            .unwrap();
        let writer = store.clone();
        let finish = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            writer.finish_release_job("firmware".into()).await.unwrap();
        });
        let staging = Staging::new(store.clone(), Behavior::Stop);
        let boundary = wait_for_idle_boundary(store.clone(), &staging, Duration::from_secs(2))
            .await
            .unwrap();
        finish.await.unwrap();
        assert!(staging.stopped.load(Ordering::SeqCst));
        drop(boundary);
        store.shutdown(Duration::from_secs(1)).await.unwrap();
    });
}

#[test]
fn failed_or_hanging_staging_stop_never_grants_boundary_and_restores_admission() {
    run(async {
        for behavior in [Behavior::Fail, Behavior::Hang] {
            let database = Database::new();
            let store = database.open();
            let staging = Staging::new(store.clone(), behavior);
            assert!(
                wait_for_idle_boundary(store.clone(), &staging, Duration::from_millis(40))
                    .await
                    .is_err()
            );
            assert!(!staging.stopped.load(Ordering::SeqCst));
            assert_eq!(
                store
                    .storage_retention_status()
                    .await
                    .unwrap()
                    .new_session_admission,
                StorageAdmissionState::Available
            );
            store.shutdown(Duration::from_secs(1)).await.unwrap();
        }
    });
}

#[test]
fn expired_boundary_and_reopened_process_cannot_authorize_activation() {
    run(async {
        let database = Database::new();
        let store = database.open();
        let staging = Staging::new(store.clone(), Behavior::Stop);
        let boundary = wait_for_idle_boundary(store.clone(), &staging, Duration::from_millis(40))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(boundary.validate().await.is_err());
        store.write_atomic(AtomicStoreWrite::empty()).await.unwrap();
        store.shutdown(Duration::from_secs(1)).await.unwrap();
        assert!(boundary.validate().await.is_err());
    });
}
