mod record_preservation;
mod rejected_fallback;

use super::*;
use crate::{
    supervisor::{
        Request,
        audit::{Compatibility, Decision, Drain, Health, PromotionOutcome},
        promotion::{self, ProcessFuture, Start},
    },
    test_support::Fixture,
};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
};
use uob_application::OperationalStore;
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
    let record = promotion::Record {
        candidate: f.artifact.digest().into(),
        previous: f.previous.compatibility.artifact_digest.as_str().into(),
        configuration_digest: qualification::digest(&fs::read(&p.configuration).unwrap()),
        production_inputs_digest: qualification::digest(&serde_json::to_vec(&p).unwrap()),
        database_device: meta.dev(),
        database_inode: meta.ino(),
        step: promotion::Step::Probation,
        recovery_attempted: false,
    };
    manager
        .ledger
        .record_promotion(
            record.clone(),
            Code::Ok,
            Decision::Promote {
                candidate_digest: record.candidate,
                previous_good_digest: Some(record.previous),
                evidence_digest: None,
                configuration_digest: Some(record.configuration_digest),
                compatibility: Compatibility::Accepted,
                drain: Drain::Granted,
                health: Health::Probation,
                outcome: PromotionOutcome::Continuing,
            },
        )
        .unwrap();
    (f, manager, p)
}

#[test]
fn status_reports_live_journal_pointers_after_rollback() {
    let _serial = crate::TEST_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    run(async {
        let (f, mut manager, _) = setup().await;
        trigger(&mut manager);
        let mut process = Process::default();
        assert_eq!(manager.rollback_automatically(&mut process).await, Code::Ok);

        let response = manager.handle(100, Request::Status {});
        let status = response.status.unwrap();
        let activation = response.activation.unwrap();
        let production = activation.production.unwrap();
        let candidate = activation.candidate.unwrap();
        assert_eq!(status.promotion.unwrap().step, promotion::Step::Probation);
        assert_eq!(
            production.digest,
            f.previous.compatibility.artifact_digest.as_str()
        );
        assert_eq!(production.phase, Phase::PreviousGood);
        assert_eq!(
            activation.previous_good.unwrap(),
            f.previous.compatibility.artifact_digest.as_str()
        );
        assert_eq!(candidate.digest, f.artifact.digest());
        assert_eq!(candidate.phase, Phase::Quarantined);

        let forbidden = manager.handle(102, Request::Status {});
        assert_eq!(forbidden.code, Code::Forbidden);
        assert!(forbidden.activation.is_none());
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
        let decision = Decision::Promote {
            candidate_digest: interrupted.candidate.clone(),
            previous_good_digest: Some(interrupted.previous.clone()),
            evidence_digest: None,
            configuration_digest: Some(interrupted.configuration_digest.clone()),
            compatibility: Compatibility::Accepted,
            drain: Drain::Granted,
            health: Health::NotObserved,
            outcome: PromotionOutcome::Continuing,
        };
        manager
            .ledger
            .record_promotion(interrupted, Code::Ok, decision)
            .unwrap();
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
