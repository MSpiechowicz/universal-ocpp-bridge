use super::*;
use crate::{
    supervisor::{
        Request,
        promotion::{self, ProcessFuture, Start},
    },
    test_support::Fixture,
};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
};
use uob_application::{
    AtomicStoreWrite, CommittedRecord, CommittedRecordId, CommittedRecordQuery, Durability,
    OperationalStore, PageLimit,
};
type Store = uob_storage_adapter::SqliteOperationalStore<String, String, String, String>;
const OLD: &[u8] = b"#!/bin/sh\n# previous\nexit 0\n";
const NEW: &[u8] = b"#!/bin/sh\n# candidate\nexit 0\n";
fn run(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(future);
}
#[derive(Default)]
struct Process {
    stops: usize,
    starts: usize,
    fail: bool,
    hang: bool,
    start: Option<Start>,
}
impl ProductionProcess for Process {
    fn stop_and_confirm(&mut self) -> ProcessFuture<'_> {
        Box::pin(async move {
            self.stops += 1;
            Ok(())
        })
    }
    fn start_and_confirm(&mut self, start: Start) -> ProcessFuture<'_> {
        Box::pin(async move {
            self.starts += 1;
            self.start = Some(start);
            if self.hang {
                std::future::pending::<()>().await;
            }
            if self.fail { Err(rejected()) } else { Ok(()) }
        })
    }
}
struct Staging;
impl failures::StagingStop for Staging {
    fn stop_and_confirm(&mut self) -> bool {
        true
    }
}
fn observation(id: u64, signal: failures::Signal) -> failures::Observation {
    failures::Observation {
        id,
        at_seconds: id,
        signal,
        resource_pressure: false,
    }
}
fn trigger(manager: &mut Supervisor) {
    for (id, signal) in [
        (1, failures::Signal::Started { invocation: 1 }),
        (
            2,
            failures::Signal::FatalInvariant {
                data_valid_and_compatible: true,
            },
        ),
    ] {
        manager
            .observe_failure(
                failures::Policy::default(),
                observation(id, signal),
                &mut Staging,
            )
            .unwrap();
    }
}
async fn setup() -> (Fixture, Supervisor, preflight::Policy) {
    let f = Fixture::with_binaries(Some(OLD), Some(NEW));
    let backups = f.state.join("backups");
    fs::create_dir(&backups).unwrap();
    fs::set_permissions(&backups, fs::Permissions::from_mode(0o700)).unwrap();
    let p = preflight::Policy {
        configuration: f.artifact.root.join("production.toml"),
        operational_database: f.artifact.root.join("production.sqlite"),
        expected_formats: f.artifact.policy.current_formats,
        service_uid: rustix::process::geteuid().as_raw(),
        service_gid: rustix::process::getegid().as_raw(),
        maximum_backup_bytes: 4 * 1024 * 1024,
        timeout_seconds: 2,
    };
    fs::write(&p.configuration, "current compatible config").unwrap();
    let store = Store::open(&p.operational_database, 16).unwrap();
    store.shutdown(DEADLINE).await.unwrap();
    drop(store);
    let mut manager = f.manager().with_preflight_policy(p.clone()).unwrap();
    assert_eq!(manager.handle(100, f.publish()).code, Code::Ok);
    for t in [Transition::BeginPromotion, Transition::BeginProbation] {
        manager
            .activation
            .transition(t, &f.artifact.policy)
            .unwrap();
    }
    let meta = fs::metadata(&p.operational_database).unwrap();
    manager
        .ledger
        .record_promotion(promotion::Record {
            candidate: f.artifact.digest().into(),
            previous: f.previous.compatibility.artifact_digest.as_str().into(),
            configuration_digest: qualification::digest(&fs::read(&p.configuration).unwrap()),
            production_inputs_digest: qualification::digest(&serde_json::to_vec(&p).unwrap()),
            database_device: meta.dev(),
            database_inode: meta.ino(),
            step: promotion::Step::Probation,
            recovery_attempted: false,
        })
        .unwrap();
    (f, manager, p)
}
#[test]
fn fallback_preserves_post_promotion_records_export_cursor_and_audits_across_reboot() {
    let _serial = crate::TEST_SERIAL.lock().unwrap();
    run(async {
        let (f, mut manager, p) = setup().await;
        let store = Store::open(&p.operational_database, 16).unwrap();
        let mut write = AtomicStoreWrite::empty();
        for name in [
            "transaction",
            "meter",
            "command",
            "delivery",
            "export-checkpoint",
            "release-audit",
        ] {
            write.committed_records.push(CommittedRecord {
                record_id: CommittedRecordId::new(name).unwrap(),
                durability: Durability::Critical,
                committed_at: serde_json::from_str("\"2026-09-12T12:00:00Z\"").unwrap(),
                record: name.to_owned(),
            });
        }
        store.write_atomic(write).await.unwrap();
        let query = || CommittedRecordQuery {
            after: None,
            limit: PageLimit::new(10).unwrap(),
            include_best_effort_telemetry: true,
        };
        let before = store.read_committed_records(query()).await.unwrap();
        store.shutdown(DEADLINE).await.unwrap();
        drop(store);
        let bytes = fs::read(&p.operational_database).unwrap();
        trigger(&mut manager);
        let audit = serde_json::to_vec(&manager.ledger.status().last_operation).unwrap();
        drop(manager); // persisted trigger is sufficient with API and browser unavailable
        let mut manager = f.manager().with_preflight_policy(p.clone()).unwrap();
        let mut process = Process::default();
        assert_eq!(manager.rollback_automatically(&mut process).await, Code::Ok);
        assert_eq!((process.stops, process.starts), (1, 1));
        assert_eq!(
            process.start.unwrap().digest,
            f.previous.compatibility.artifact_digest.as_str()
        );
        assert_eq!(fs::read(&p.operational_database).unwrap(), bytes);
        let store = Store::open(&p.operational_database, 16).unwrap();
        let after = store.read_committed_records(query()).await.unwrap();
        assert_eq!(before, after);
        store.shutdown(DEADLINE).await.unwrap();
        assert_eq!(
            serde_json::to_vec(&manager.ledger.status().last_operation).unwrap(),
            audit
        );
        assert_eq!(
            manager.activation.state().candidate.as_ref().unwrap().phase,
            Phase::Quarantined
        );
        drop(manager);
        let mut manager = f.manager().with_preflight_policy(p).unwrap();
        let mut process = Process::default();
        assert_eq!(manager.rollback_automatically(&mut process).await, Code::Ok);
        assert_eq!((process.stops, process.starts), (0, 0));
        assert_eq!(
            manager
                .handle(
                    100,
                    Request::Stage {
                        digest: f.artifact.digest().into()
                    }
                )
                .code,
            Code::RecoveryRequired
        );
        assert_eq!(
            manager
                .observe_failure_and_rollback(
                    failures::Policy::default(),
                    observation(3, failures::Signal::Watchdog),
                    &mut Staging,
                    &mut process
                )
                .await
                .unwrap(),
            Code::RecoveryRequired
        );
        assert_eq!(
            manager.handle(100, Request::Status {}).code,
            Code::RecoveryRequired
        );
    });
}
#[test]
fn failed_or_interrupted_fallback_is_never_retried_after_reboot() {
    let _serial = crate::TEST_SERIAL.lock().unwrap();
    run(async {
        for interrupted in [false, true] {
            let (f, mut manager, p) = setup().await;
            trigger(&mut manager);
            let mut process = Process {
                fail: !interrupted,
                hang: interrupted,
                ..Process::default()
            };
            if interrupted {
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(500),
                        manager.rollback_automatically(&mut process)
                    )
                    .await
                    .is_err()
                );
            } else {
                assert_eq!(
                    manager.rollback_automatically(&mut process).await,
                    Code::RecoveryRequired
                );
            }
            assert_eq!(process.starts, 1);
            drop(manager);
            let mut manager = f.manager().with_preflight_policy(p).unwrap();
            let mut process = Process::default();
            assert_eq!(
                manager.rollback_automatically(&mut process).await,
                Code::RecoveryRequired
            );
            assert_eq!((process.stops, process.starts), (0, 0));
            assert_eq!(
                manager.activation.state().candidate.as_ref().unwrap().phase,
                Phase::Quarantined
            );
        }
    });
}
#[test]
fn unsafe_eligibility_or_data_never_starts_a_fallback() {
    let _serial = crate::TEST_SERIAL.lock().unwrap();
    run(async {
        for bad in [
            "revoked", "floor", "config", "database", "corrupt", "evidence", "formats",
        ] {
            let (f, mut manager, p) = setup().await;
            trigger(&mut manager);
            match bad {
                "revoked" => {
                    manager
                        .policy
                        .security
                        .revoked_artifacts
                        .insert(f.previous.compatibility.artifact_digest.clone());
                }
                "floor" => manager.policy.security.minimum_release_sequence = u64::MAX,
                "config" => fs::write(&p.configuration, "changed").unwrap(),
                "database" => {
                    fs::rename(
                        &p.operational_database,
                        p.operational_database.with_extension("old"),
                    )
                    .unwrap();
                    fs::write(&p.operational_database, "replacement").unwrap();
                }
                "corrupt" => fs::write(&p.operational_database, "corrupt").unwrap(),
                "formats" => {
                    manager.policy.current_formats.external_database =
                        crate::SchemaVersion::new(999);
                }
                "evidence" => {
                    let q = manager.ledger.status().qualification.as_ref().unwrap();
                    fs::write(
                        f.state
                            .join("evidence")
                            .join(format!("{}.sig", q.evidence_digest)),
                        [0; 64],
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
            let mut process = Process::default();
            assert_eq!(
                manager.rollback_automatically(&mut process).await,
                Code::RecoveryRequired,
                "{bad}"
            );
            assert_eq!(process.starts, 0, "{bad}");
        }
    });
}
#[test]
fn missing_fallback_and_uncertain_ledger_are_actionable_without_process_calls() {
    let _serial = crate::TEST_SERIAL.lock().unwrap();
    run(async {
        let (f, mut manager, _) = setup().await;
        trigger(&mut manager);
        fs::write(f.state.join("state.next"), "interrupted").unwrap();
        drop(manager);
        let mut manager = f.manager();
        let mut process = Process::default();
        assert_eq!(
            manager.rollback_automatically(&mut process).await,
            Code::RecoveryRequired
        );
        assert_eq!(process.stops, 0);
        drop(manager);
        // A fresh first installation has no journal production/fallback observation.
        let f = crate::test_support::artifacts::Fixture::new();
        let store = f.root.join("store");
        let state = f.root.join("state");
        for dir in [&store, &state] {
            fs::create_dir(dir).unwrap();
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut manager = Supervisor::open(
            &state,
            &store,
            f.policy.clone(),
            vec![super::super::Grant {
                uid: 100,
                permissions: vec![super::super::Permission::Read],
            }],
        )
        .unwrap();
        trigger(&mut manager);
        assert_eq!(
            manager.rollback_automatically(&mut process).await,
            Code::RecoveryRequired
        );
        assert_eq!(
            manager.ledger.status().rollback.as_ref().unwrap().reason,
            Reason::NoPreviousGood
        );
        assert_eq!(
            manager.handle(100, Request::Status {}).code,
            Code::RecoveryRequired
        );
        assert_eq!(process.stops, 0);
    });
}

#[test]
fn trusted_observation_rolls_back_automatically_but_outages_never_stop_production() {
    let _serial = crate::TEST_SERIAL.lock().unwrap();
    run(async {
        let (_f, mut manager, _p) = setup().await;
        let mut interrupted = manager.ledger.status().promotion.clone().unwrap();
        interrupted.step = promotion::Step::Starting;
        manager.ledger.record_promotion(interrupted).unwrap();
        let mut process = Process::default();
        for (id, signal) in [
            (1, failures::Signal::Started { invocation: 1 }),
            (2, failures::Signal::MqttOutage),
            (3, failures::Signal::ExternalDatabaseOutage),
        ] {
            assert_eq!(
                manager
                    .observe_failure_and_rollback(
                        failures::Policy::default(),
                        observation(id, signal),
                        &mut Staging,
                        &mut process
                    )
                    .await
                    .unwrap(),
                Code::Ok
            );
        }
        assert_eq!((process.stops, process.starts), (0, 0));
        assert_eq!(
            manager
                .observe_failure_and_rollback(
                    failures::Policy::default(),
                    observation(
                        4,
                        failures::Signal::FatalInvariant {
                            data_valid_and_compatible: true
                        }
                    ),
                    &mut Staging,
                    &mut process
                )
                .await
                .unwrap(),
            Code::Ok
        );
        assert_eq!((process.stops, process.starts), (1, 1));
        assert_eq!(manager.handle(100, Request::Status {}).code, Code::Ok);
        assert_eq!(
            manager.ledger.status().rollback.as_ref().unwrap().step,
            Step::Restored
        );
    });
}
