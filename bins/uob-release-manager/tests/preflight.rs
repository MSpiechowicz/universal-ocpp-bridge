#[allow(dead_code)]
#[path = "qualification/support.rs"]
mod support;
use std::{fs, os::unix::fs::PermissionsExt};
use support::Fixture;
use uob_release_manager::supervisor::{
    Code, Request,
    preflight::{BackupMetadata, Policy},
};
use uob_storage_adapter::SqliteOperationalStore;

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn policy(f: &Fixture) -> Policy {
    let backups = f.state.join("backups");
    fs::create_dir(&backups).unwrap();
    fs::set_permissions(&backups, fs::Permissions::from_mode(0o700)).unwrap();
    let configuration = f.artifact.root.join("production.toml");
    fs::write(&configuration, "[bridge]\nid='production'\n").unwrap();
    let operational_database = f.artifact.root.join("production.sqlite");
    let store = SqliteOperationalStore::<(), (), (), ()>::open(&operational_database, 1).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(store.shutdown(std::time::Duration::from_secs(2)))
        .unwrap();
    Policy {
        configuration,
        operational_database,
        expected_formats: f.artifact.policy.current_formats,
        service_uid: rustix::process::geteuid().as_raw(),
        service_gid: rustix::process::getegid().as_raw(),
        maximum_backup_bytes: f.artifact.policy.backup_reserve_bytes,
        timeout_seconds: 2,
    }
}

#[test]
fn qualified_preflight_creates_separate_verified_backup_without_switching_live_data() {
    let _serial = SERIAL.lock().unwrap();
    // Signed fixture verifies the exact offline-check invocation, with no bridge process.
    let f = Fixture::with_candidate(Some(b"#!/bin/sh\n[ \"$1\" = config ] && [ \"$2\" = check ] && [ \"$3\" = --config ] && [ -r \"$4\" ] && [ \"$5\" = --secrets ]\n"));
    let p = policy(&f);
    let live = fs::read(&p.operational_database).unwrap();
    let active = fs::read(f.store.join("active")).unwrap();
    let mut manager = f.manager().with_preflight_policy(p.clone()).unwrap();
    let promote = Request::Promote {
        digest: f.artifact.digest().into(),
    };
    assert_eq!(
        manager.handle(100, promote.clone()).code,
        Code::QualificationRequired
    );
    assert_eq!(manager.handle(100, f.publish()).code, Code::Ok);
    assert_eq!(manager.handle(101, promote.clone()).code, Code::Forbidden);
    assert!(!f.state.join("backups/metadata.json").exists());
    assert_eq!(
        manager.handle(100, promote.clone()).code,
        Code::ActivationBlocked
    );
    let metadata: BackupMetadata =
        serde_json::from_slice(&fs::read(f.state.join("backups/metadata.json")).unwrap()).unwrap();
    assert_eq!(metadata.candidate_digest, f.artifact.digest());
    assert_eq!(
        metadata.previous_digest,
        f.previous.compatibility.artifact_digest.as_str()
    );
    assert!(metadata.bytes > 0);
    assert_eq!(metadata.expected_formats, p.expected_formats);
    assert_eq!(fs::read(&p.operational_database).unwrap(), live);
    assert_eq!(fs::read(f.store.join("active")).unwrap(), active);
    drop(manager);
    let mut manager = f.manager().with_preflight_policy(p).unwrap();
    assert_eq!(manager.handle(100, promote).code, Code::PreflightRejected);
    assert_eq!(
        manager.handle(100, Request::Rollback {}).code,
        Code::QualificationRequired
    );
    assert_eq!(fs::read(f.store.join("active")).unwrap(), active);
}

#[test]
fn invalid_config_unproven_formats_and_backup_failure_block_preflight() {
    let _serial = SERIAL.lock().unwrap();
    for failure in [
        "config",
        "formats",
        "database",
        "capacity",
        "missing-config",
        "timeout",
    ] {
        let f = Fixture::with_candidate(Some(match failure {
            "config" => b"#!/bin/sh\nexit 2\n",
            "timeout" => b"#!/bin/sh\nwhile :; do :; done\n",
            _ => b"#!/bin/sh\nexit 0\n",
        }));
        let mut p = policy(&f);
        match failure {
            "formats" => {
                p.expected_formats.external_database = uob_release_manager::SchemaVersion::new(999);
            }
            "database" => fs::write(&p.operational_database, "corrupt").unwrap(),
            "capacity" => p.maximum_backup_bytes = 1,
            "missing-config" => fs::remove_file(&p.configuration).unwrap(),
            _ => (),
        }
        let mut manager = f.manager().with_preflight_policy(p).unwrap();
        assert_eq!(manager.handle(100, f.publish()).code, Code::Ok);
        let active = fs::read(f.store.join("active")).unwrap();
        assert_eq!(
            manager
                .handle(
                    100,
                    Request::Promote {
                        digest: f.artifact.digest().into()
                    }
                )
                .code,
            Code::PreflightRejected,
            "{failure}"
        );
        assert!(!f.state.join("backups/metadata.json").exists(), "{failure}");
        assert_eq!(fs::read(f.store.join("active")).unwrap(), active);
    }
}
