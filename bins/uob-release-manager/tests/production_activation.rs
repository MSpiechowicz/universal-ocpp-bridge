#[path = "production_activation/support.rs"]
mod support;
// Subprocess fork/exec briefly inherits other threads' file locks. Serialize fixtures
// so a transient inherited installer lock is not mistaken for activation contention.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
use std::{fs, net::TcpListener, os::unix::fs::MetadataExt, sync::Arc, time::Duration};
use support::{BINARY, Behavior, Fixture, Process, Store, policy, run, seed, staging};
use uob_application::OperationalStore;
use uob_release_manager::{
    activation::ActivationJournal,
    supervisor::{
        Code, Request,
        promotion::{Host, Step},
    },
};

#[test]
#[ignore = "subprocess socket and ownership fixture"]
fn production_child() {
    let Some(root) = std::env::var_os("UOB_ACTIVATION_CHILD_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("service.lock"))
        .unwrap();
    lock.try_lock().expect("only one production service owner");
    let address = fs::read_to_string(root.join("address")).unwrap_or_else(|_| "127.0.0.1:0".into());
    let listener = TcpListener::bind(address).expect("only one charger socket owner");
    fs::write(
        root.join("address"),
        listener.local_addr().unwrap().to_string(),
    )
    .unwrap();
    fs::write(root.join("ready"), b"ready").unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
fn activation_stops_old_socket_owner_and_reopens_production_data_in_probation() {
    let _serial = SERIAL.lock().unwrap();
    run(async {
        let f = Fixture::with_candidate(Some(BINARY));
        let p = policy(&f);
        let store = Arc::new(Store::open(&p.operational_database, 16).unwrap());
        seed(&store).await;
        let inode = fs::metadata(&p.operational_database).unwrap().ino();
        let config = fs::read(&p.configuration).unwrap();
        fs::write(f.artifact.root.join("staging.sqlite"), "never import this").unwrap();
        let mut process = Process::new(&f, store.clone());
        process.boot_old().await;
        let old_pid = process.child.as_ref().unwrap().id();
        let address = fs::read_to_string(f.artifact.root.join("address")).unwrap();
        let mut manager = f.manager().with_preflight_policy(p.clone()).unwrap();
        assert_eq!(manager.handle(100, f.publish()).code, Code::Ok);
        assert!(ActivationJournal::open(&f.store, &f.artifact.policy).is_err());
        let result = manager
            .promote_at_idle(
                100,
                f.artifact.digest().into(),
                Host {
                    drain: store,
                    staging: &staging(),
                    production: &mut process,
                    maintenance_window: Duration::from_secs(2),
                },
            )
            .await;
        assert_eq!(result.code, Code::Ok);
        assert_eq!((process.stops, process.starts), (1, 1));
        assert_ne!(process.child.as_ref().unwrap().id(), old_pid);
        assert_eq!(
            fs::read_to_string(f.artifact.root.join("address")).unwrap(),
            address
        );
        assert!(
            TcpListener::bind(address).is_err(),
            "candidate owns the reconnected endpoint"
        );
        assert_eq!(fs::metadata(&p.operational_database).unwrap().ino(), inode);
        assert_eq!(fs::read(&p.configuration).unwrap(), config);
        assert_eq!(process.store.reserve_transaction_id().await.unwrap(), 2);
        assert_eq!(
            fs::read_to_string(f.store.join("active")).unwrap(),
            f.artifact.digest()
        );
        assert_eq!(
            fs::read_to_string(f.store.join("previous-good")).unwrap(),
            f.previous.compatibility.artifact_digest.as_str()
        );
        let status = manager.handle(100, Request::Status {}).status.unwrap();
        assert_eq!(status.promotion.unwrap().step, Step::Probation);
        drop(manager);
        let journal = ActivationJournal::open(&f.store, &f.artifact.policy).unwrap();
        assert_eq!(
            journal.state().production.as_ref().unwrap().phase,
            uob_release_manager::activation::Phase::Probation
        );
        assert!(ActivationJournal::open(&f.store, &f.artifact.policy).is_err());
        process
            .store
            .shutdown(Duration::from_secs(1))
            .await
            .unwrap();
    });
}

#[test]
fn permissions_and_missing_qualification_never_touch_process_or_backup() {
    let _serial = SERIAL.lock().unwrap();
    run(async {
        let f = Fixture::with_candidate(Some(BINARY));
        let p = policy(&f);
        let store = Arc::new(Store::open(&p.operational_database, 16).unwrap());
        let mut process = Process::new(&f, store.clone());
        let mut manager = f.manager().with_preflight_policy(p).unwrap();
        for (uid, code) in [(101, Code::Forbidden), (100, Code::QualificationRequired)] {
            let response = manager
                .promote_at_idle(
                    uid,
                    f.artifact.digest().into(),
                    Host {
                        drain: store.clone(),
                        staging: &staging(),
                        production: &mut process,
                        maintenance_window: Duration::from_secs(1),
                    },
                )
                .await;
            assert_eq!(response.code, code);
        }
        assert_eq!((process.stops, process.starts), (0, 0));
        assert!(!f.state.join("backups/metadata.json").exists());
        store.shutdown(Duration::from_secs(1)).await.unwrap();
    });
}

#[test]
fn failure_or_timeout_before_confirmed_stop_never_switches_and_failure_after_switch_is_durable() {
    let _serial = SERIAL.lock().unwrap();
    run(async {
        for behavior in [Behavior::FailStop, Behavior::HangStop, Behavior::FailStart] {
            let f = Fixture::with_candidate(Some(BINARY));
            let p = policy(&f);
            let store = Arc::new(Store::open(&p.operational_database, 16).unwrap());
            seed(&store).await;
            let mut process = Process::new(&f, store.clone());
            process.behavior = behavior;
            let mut manager = f.manager().with_preflight_policy(p.clone()).unwrap();
            assert_eq!(manager.handle(100, f.publish()).code, Code::Ok);
            let response = manager
                .promote_at_idle(
                    100,
                    f.artifact.digest().into(),
                    Host {
                        drain: store.clone(),
                        staging: &staging(),
                        production: &mut process,
                        maintenance_window: Duration::from_millis(100),
                    },
                )
                .await;
            assert_eq!(response.code, Code::RecoveryRequired);
            let expected = if behavior == Behavior::FailStart {
                f.artifact.digest()
            } else {
                f.previous.compatibility.artifact_digest.as_str()
            };
            assert_eq!(
                fs::read_to_string(f.store.join("active")).unwrap(),
                expected
            );
            drop(manager);
            let mut manager = f.manager().with_preflight_policy(p).unwrap();
            let status = manager.handle(100, Request::Status {});
            assert_eq!(status.code, Code::RecoveryRequired);
            assert_eq!(
                status.status.unwrap().promotion.unwrap().step,
                Step::RecoveryRequired
            );
            let stops = process.stops;
            assert_eq!(
                manager.recover_activation(100, &mut process).await.code,
                Code::RecoveryRequired
            );
            assert_eq!(
                process.stops, stops,
                "failed operation is not silently retried"
            );
            store.shutdown(Duration::from_secs(1)).await.unwrap();
        }
    });
}

#[test]
fn changed_production_configuration_and_staging_failure_prevent_switch() {
    let _serial = SERIAL.lock().unwrap();
    run(async {
        for changed in [false, true] {
            let f = Fixture::with_candidate(Some(BINARY));
            let p = policy(&f);
            let store = Arc::new(Store::open(&p.operational_database, 16).unwrap());
            let mut process = Process::new(&f, store.clone());
            let mut manager = f.manager().with_preflight_policy(p.clone()).unwrap();
            assert_eq!(manager.handle(100, f.publish()).code, Code::Ok);
            let mut staging = staging();
            staging.fail = !changed;
            staging.change_configuration = changed.then_some(p.configuration);
            let response = manager
                .promote_at_idle(
                    100,
                    f.artifact.digest().into(),
                    Host {
                        drain: store.clone(),
                        staging: &staging,
                        production: &mut process,
                        maintenance_window: Duration::from_secs(1),
                    },
                )
                .await;
            assert_eq!(
                response.code,
                if changed {
                    Code::RecoveryRequired
                } else {
                    Code::ActivationBlocked
                }
            );
            assert_eq!((process.stops, process.starts), (0, 0));
            assert_eq!(
                fs::read_to_string(f.store.join("active")).unwrap(),
                f.previous.compatibility.artifact_digest.as_str()
            );
            store.shutdown(Duration::from_secs(1)).await.unwrap();
        }
    });
}

#[test]
fn cancelled_start_recovers_selected_artifact_once_without_a_new_drain_or_switch() {
    let _serial = SERIAL.lock().unwrap();
    run(async {
        let f = Fixture::with_candidate(Some(BINARY));
        let p = policy(&f);
        let store = Arc::new(Store::open(&p.operational_database, 16).unwrap());
        seed(&store).await;
        let mut process = Process::new(&f, store.clone());
        process.behavior = Behavior::HangStart;
        let mut manager = f.manager().with_preflight_policy(p.clone()).unwrap();
        assert_eq!(manager.handle(100, f.publish()).code, Code::Ok);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(300),
                manager.promote_at_idle(
                    100,
                    f.artifact.digest().into(),
                    Host {
                        drain: store,
                        staging: &staging(),
                        production: &mut process,
                        maintenance_window: Duration::from_secs(1)
                    }
                )
            )
            .await
            .is_err()
        );
        assert_eq!(
            manager
                .handle(100, Request::Status {})
                .status
                .unwrap()
                .promotion
                .unwrap()
                .step,
            Step::Starting
        );
        drop(manager);
        let mut manager = f.manager().with_preflight_policy(p.clone()).unwrap();
        assert_eq!(
            manager.handle(100, Request::Status {}).code,
            Code::RecoveryRequired
        );
        assert_eq!(
            manager.recover_activation(101, &mut process).await.code,
            Code::Forbidden
        );
        process.behavior = Behavior::Normal;
        assert_eq!(
            manager.recover_activation(100, &mut process).await.code,
            Code::Ok
        );
        assert_eq!(process.store.reserve_transaction_id().await.unwrap(), 2);
        let record = manager
            .handle(100, Request::Status {})
            .status
            .unwrap()
            .promotion
            .unwrap();
        assert_eq!(record.step, Step::Probation);
        assert!(record.recovery_attempted);
        drop(manager);
        let mut manager = f.manager().with_preflight_policy(p).unwrap();
        let stops = process.stops;
        assert_eq!(
            manager.recover_activation(100, &mut process).await.code,
            Code::RecoveryRequired
        );
        assert_eq!(process.stops, stops);
        process
            .store
            .shutdown(Duration::from_secs(1))
            .await
            .unwrap();
    });
}

#[test]
fn cancellation_during_stop_recovers_only_previous_artifact_and_defers_promotion() {
    let _serial = SERIAL.lock().unwrap();
    run(async {
        let previous = [BINARY, b"\n# previous artifact\n"].concat();
        let f = Fixture::with_binaries(Some(&previous), Some(BINARY));
        let p = policy(&f);
        let store = Arc::new(Store::open(&p.operational_database, 16).unwrap());
        seed(&store).await;
        let mut process = Process::new(&f, store.clone());
        process.boot_old().await;
        process.behavior = Behavior::HangStop;
        let mut manager = f.manager().with_preflight_policy(p.clone()).unwrap();
        assert_eq!(manager.handle(100, f.publish()).code, Code::Ok);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(300),
                manager.promote_at_idle(
                    100,
                    f.artifact.digest().into(),
                    Host {
                        drain: store,
                        staging: &staging(),
                        production: &mut process,
                        maintenance_window: Duration::from_secs(2)
                    }
                )
            )
            .await
            .is_err()
        );
        drop(manager);
        let mut manager = f.manager().with_preflight_policy(p).unwrap();
        process.behavior = Behavior::Normal;
        assert_eq!(
            manager.recover_activation(100, &mut process).await.code,
            Code::Ok
        );
        assert_eq!(
            process.start.as_ref().unwrap().digest,
            f.previous.compatibility.artifact_digest.as_str()
        );
        assert_eq!(
            fs::read_to_string(f.store.join("active")).unwrap(),
            f.previous.compatibility.artifact_digest.as_str()
        );
        let status = manager.handle(100, Request::Status {});
        assert_eq!(status.code, Code::RecoveryRequired);
        assert_eq!(
            status.status.unwrap().promotion.unwrap().step,
            Step::RecoveredPrevious
        );
        process
            .store
            .shutdown(Duration::from_secs(1))
            .await
            .unwrap();
    });
}

#[test]
fn replacing_database_during_drain_is_detected_before_stopping_production() {
    let _serial = SERIAL.lock().unwrap();
    run(async {
        let f = Fixture::with_candidate(Some(BINARY));
        let p = policy(&f);
        let store = Arc::new(Store::open(&p.operational_database, 16).unwrap());
        let mut process = Process::new(&f, store.clone());
        let mut manager = f.manager().with_preflight_policy(p.clone()).unwrap();
        assert_eq!(manager.handle(100, f.publish()).code, Code::Ok);
        let mut staging = staging();
        staging.replace_database = Some(p.operational_database);
        assert_eq!(
            manager
                .promote_at_idle(
                    100,
                    f.artifact.digest().into(),
                    Host {
                        drain: store.clone(),
                        staging: &staging,
                        production: &mut process,
                        maintenance_window: Duration::from_secs(2)
                    }
                )
                .await
                .code,
            Code::RecoveryRequired
        );
        assert_eq!((process.stops, process.starts), (0, 0));
        assert_eq!(
            fs::read_to_string(f.store.join("active")).unwrap(),
            f.previous.compatibility.artifact_digest.as_str()
        );
        store.shutdown(Duration::from_secs(1)).await.unwrap();
    });
}
