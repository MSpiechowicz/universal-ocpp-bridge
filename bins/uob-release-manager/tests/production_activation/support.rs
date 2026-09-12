#[allow(dead_code)]
#[path = "../qualification/support.rs"]
pub mod qualification_fixture;
pub use qualification_fixture::Fixture;
use std::{
    fs,
    future::Future,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    pin::Pin,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};
use uob_application::{OperationalStore, PageLimit, RecoveryQuery, StorageError};
use uob_release_manager::{
    artifacts::InstallError,
    drain::StagingStopPort,
    supervisor::{
        preflight::Policy,
        promotion::{ProcessFuture, ProductionProcess, Start},
    },
};
pub type Store = uob_storage_adapter::SqliteOperationalStore<String, String, String, String>;
pub const BINARY: &[u8] = b"#!/bin/sh\nif [ \"$1\" = config ]; then exit 0; fi\nexec \"$UOB_TEST_EXECUTABLE\" --exact production_child --ignored --nocapture\n";
pub fn run(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(future);
}
pub struct Staging {
    pub change_configuration: Option<PathBuf>,
    pub replace_database: Option<PathBuf>,
    pub fail: bool,
}
impl StagingStopPort for Staging {
    fn stop_and_confirm(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send + '_>> {
        Box::pin(async move {
            if let Some(path) = &self.change_configuration {
                fs::write(path, "changed").unwrap();
            }
            if let Some(path) = &self.replace_database {
                fs::rename(path, path.with_extension("original")).unwrap();
                fs::write(path, "replacement database").unwrap();
            }
            if self.fail {
                return Err(StorageError::new(
                    uob_application::StorageErrorCode::Unavailable,
                    "test stop failure",
                ));
            }
            Ok(())
        })
    }
}
pub fn staging() -> Staging {
    Staging {
        change_configuration: None,
        replace_database: None,
        fail: false,
    }
}
pub fn policy(f: &Fixture) -> Policy {
    let backups = f.state.join("backups");
    fs::create_dir(&backups).unwrap();
    fs::set_permissions(&backups, fs::Permissions::from_mode(0o700)).unwrap();
    let configuration = f.artifact.root.join("production.toml");
    fs::write(&configuration, "[bridge]\nid='production'\n").unwrap();
    Policy {
        configuration,
        operational_database: f.artifact.root.join("production.sqlite"),
        expected_formats: f.artifact.policy.current_formats,
        service_uid: rustix::process::geteuid().as_raw(),
        service_gid: rustix::process::getegid().as_raw(),
        maximum_backup_bytes: f.artifact.policy.backup_reserve_bytes,
        timeout_seconds: 2,
    }
}
#[derive(Clone, Copy, PartialEq)]
pub enum Behavior {
    Normal,
    FailStop,
    HangStop,
    FailStart,
    HangStart,
}
pub struct Process {
    pub child: Option<Child>,
    pub store: Arc<Store>,
    pub behavior: Behavior,
    pub stops: usize,
    pub starts: usize,
    pub root: PathBuf,
    pub start: Option<Start>,
    pub phase_entered: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Process {
    pub fn new(f: &Fixture, store: Arc<Store>) -> Self {
        Self {
            child: None,
            store,
            behavior: Behavior::Normal,
            stops: 0,
            starts: 0,
            root: f.artifact.root.clone(),
            start: None,
            phase_entered: None,
        }
    }
    pub async fn boot_old(&mut self) {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "production_child", "--ignored", "--nocapture"]);
        self.spawn(command).await;
    }
    async fn spawn(&mut self, mut command: Command) {
        let ready = self.root.join("ready");
        let _ = fs::remove_file(&ready);
        command
            .env("UOB_ACTIVATION_CHILD_ROOT", &self.root)
            .env("UOB_TEST_EXECUTABLE", std::env::current_exe().unwrap())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.child = Some(command.spawn().unwrap());
        tokio::time::timeout(Duration::from_secs(3), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}
impl ProductionProcess for Process {
    fn stop_and_confirm(&mut self) -> ProcessFuture<'_> {
        Box::pin(async move {
            self.stops += 1;
            if self.behavior == Behavior::HangStop {
                if let Some(entered) = self.phase_entered.take() {
                    entered.send(()).unwrap();
                }
                std::future::pending::<()>().await;
            }
            if self.behavior == Behavior::FailStop {
                return Err(InstallError::Rejected("test stop failed"));
            }
            if let Some(mut child) = self.child.take() {
                child.kill().unwrap();
                child.wait().unwrap();
            }
            self.store.shutdown(Duration::from_secs(1)).await.unwrap();
            Ok(())
        })
    }
    fn start_and_confirm(&mut self, start: Start) -> ProcessFuture<'_> {
        Box::pin(async move {
            self.starts += 1;
            assert!(self.child.is_none(), "old socket owner must have exited");
            assert_eq!(
                start.operational_database,
                self.root.join("production.sqlite")
            );
            assert_eq!(start.configuration, self.root.join("production.toml"));
            if self.behavior == Behavior::HangStart {
                if let Some(entered) = self.phase_entered.take() {
                    entered.send(()).unwrap();
                }
                std::future::pending::<()>().await;
            }
            if self.behavior == Behavior::FailStart {
                return Err(InstallError::Rejected("test start failed"));
            }
            // Restart uses the same authoritative database; pending target data stays bound
            // to its original destination. The controller has no charger command interface.
            self.store = Arc::new(Store::open(&start.operational_database, 16).unwrap());
            let recovered = self
                .store
                .recover(RecoveryQuery {
                    limit: PageLimit::new(10).unwrap(),
                })
                .await
                .unwrap();
            assert_eq!(recovered.pending_deliveries.len(), 1);
            assert_eq!(
                recovered.pending_deliveries[0].target_instance_id.as_str(),
                "production-target"
            );
            assert_eq!(
                recovered.pending_deliveries[0].delivery_id.as_str(),
                "production-delivery"
            );
            let mut command = Command::new(&start.binary);
            command
                .args(["serve", "--config"])
                .arg(&start.configuration);
            self.spawn(command).await;
            self.start = Some(start);
            Ok(())
        })
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
pub async fn seed(store: &Store) {
    use uob_application::{AtomicStoreWrite, DeliveryId, Durability, PendingDelivery};
    use uob_contracts::{BridgeId, EventId, ResourceRef, StationId, TargetInstanceId};
    let mut write = AtomicStoreWrite::empty();
    write.required_deliveries.push(PendingDelivery {
        delivery_id: DeliveryId::new("production-delivery").unwrap(),
        event_id: EventId::new("production-event").unwrap(),
        target_instance_id: TargetInstanceId::new("production-target").unwrap(),
        target_configuration_revision: 7,
        ordering_key: ResourceRef {
            bridge_id: BridgeId::new("production-bridge").unwrap(),
            station_id: StationId::new("station").unwrap(),
            resource: None,
            native_protocol_reference: None,
        },
        deadline: serde_json::from_str("\"2026-09-12T12:00:00Z\"").unwrap(),
        durability: Durability::Critical,
        payload: "production payload".into(),
    });
    store.write_atomic(write).await.unwrap();
    assert_eq!(store.reserve_transaction_id().await.unwrap(), 1);
}

// Cancel only after the process fixture reaches the requested hanging phase. Preflight can take
// longer on CI; elapsed wall time alone does not establish which durable transition was reached.
pub async fn cancel_at_phase(
    promotion: impl Future<Output = uob_release_manager::supervisor::Response>,
    entered: tokio::sync::oneshot::Receiver<()>,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut promotion = std::pin::pin!(promotion);
        let mut entered = std::pin::pin!(entered);
        std::future::poll_fn(|context| {
            if let std::task::Poll::Ready(response) = promotion.as_mut().poll(context) {
                panic!(
                    "promotion completed before cancellation: {:?}",
                    response.code
                );
            }
            entered
                .as_mut()
                .poll(context)
                .map(|result| result.expect("process phase signal"))
        })
        .await;
    })
    .await
    .expect("promotion must reach the requested process phase");
}

// Force preflight beyond the former 300 ms cancellation timer to reproduce the CI race.
pub fn delayed_preflight_binary() -> Vec<u8> {
    std::str::from_utf8(BINARY)
        .unwrap()
        .replace("then exit 0", "then sleep 0.5; exit 0")
        .into_bytes()
}
