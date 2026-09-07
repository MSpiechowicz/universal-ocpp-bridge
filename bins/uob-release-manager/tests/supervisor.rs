#[path = "supervisor/adversarial.rs"]
mod adversarial;
#[path = "supervisor/support.rs"]
mod support;
use std::{fs, path::PathBuf};
use support::Fixture;
use uob_release_manager::supervisor::{Code, Grant, Permission, Request, Supervisor, ipc::Server};

#[test]
#[ignore = "subprocess fixture launched by IPC tests"]
fn daemon_fixture() {
    let Some(path) = std::env::var_os("UOB_SUPERVISOR_TEST_INPUT") else {
        return;
    };
    let input: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let path = |name: &str| PathBuf::from(input[name].as_str().unwrap());
    let manager = Supervisor::open(
        &path("state"),
        &path("store"),
        serde_json::from_value(input["policy"].clone()).unwrap(),
        serde_json::from_value(input["grants"].clone()).unwrap(),
    )
    .unwrap();
    Server::bind(&path("runtime"))
        .unwrap()
        .run(manager)
        .unwrap();
}

fn grant(uid: u32, permissions: &[Permission]) -> Grant {
    Grant {
        uid,
        permissions: permissions.to_vec(),
    }
}

#[test]
fn permissions_are_independent_and_fail_before_artifact_or_state_access() {
    let f = Fixture::new();
    let mut manager = f.manager(vec![
        grant(10, &[Permission::Read]),
        grant(11, &[Permission::Stage]),
        grant(12, &[Permission::Activate]),
    ]);
    let stage = Request::Stage {
        digest: f.artifacts.digest().into(),
    };
    let promote = Request::Promote {
        digest: f.artifacts.digest().into(),
    };
    for uid in [10, 99] {
        for request in [stage.clone(), promote.clone(), Request::Rollback {}] {
            assert_eq!(manager.handle(uid, request).code, Code::Forbidden);
        }
    }
    assert!(!f.state.join("state.json").exists());
    assert_eq!(manager.handle(11, promote.clone()).code, Code::Forbidden);
    assert_eq!(
        manager.handle(11, Request::Rollback {}).code,
        Code::Forbidden
    );
    assert_eq!(manager.handle(11, Request::Status {}).code, Code::Forbidden);
    assert_eq!(manager.handle(12, stage.clone()).code, Code::Forbidden);
    assert_eq!(manager.handle(11, stage).code, Code::Ok);
    assert_eq!(
        manager.handle(12, promote).code,
        Code::QualificationRequired
    );
    assert_eq!(
        manager.handle(12, Request::Rollback {}).code,
        Code::QualificationRequired
    );
    let observed = manager.handle(10, Request::Status {}).status.unwrap();
    assert_eq!(observed.sequence, 3);
    assert_eq!(observed.failed_operations, 2);
    assert_eq!(
        observed.staged_verified_digest.as_deref(),
        Some(f.artifacts.digest())
    );
    assert!(!f.store.join("active").exists());
    assert!(!f.store.join("previous-good").exists());
}

#[test]
fn real_ipc_survives_supervisor_restart_without_a_bridge_process() {
    let f = Fixture::new();
    {
        let daemon = f.launch(&[Permission::Read, Permission::Stage, Permission::Activate]);
        let response = daemon.raw(&format!(
            r#"{{"operation":"stage","digest":"{}"}}"#,
            f.artifacts.digest()
        ));
        assert_eq!(response.code, Code::Ok);
        assert_eq!(response.manager_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            daemon.raw(r#"{"operation":"rollback"}"#).code,
            Code::QualificationRequired
        );
    }
    // SIGKILL leaves a socket inode. The next exclusive owner recovers that inode,
    // and reads the same fsynced private state while no bridge/API was ever started.
    let daemon = f.launch(&[Permission::Read]);
    let response = daemon.raw(r#"{"operation":"status"}"#);
    assert_eq!(response.code, Code::Ok);
    let status = response.status.unwrap();
    assert_eq!(status.sequence, 2);
    assert_eq!(status.failed_operations, 1);
    assert_eq!(
        status.staged_verified_digest.as_deref(),
        Some(f.artifacts.digest())
    );
    assert_eq!(
        status.last_operation.unwrap().uid,
        rustix::process::geteuid().as_raw()
    );
    assert_eq!(
        daemon
            .raw(&format!(
                r#"{{"operation":"stage","digest":"{}"}}"#,
                f.artifacts.digest()
            ))
            .code,
        Code::Forbidden
    );
}

#[test]
fn stage_rechecks_bytes_and_only_selects_the_installed_candidate() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let mut manager = f.manager(vec![grant(10, &[Permission::Read, Permission::Stage])]);
    let request = Request::Stage {
        digest: f.artifacts.digest().into(),
    };
    assert_eq!(manager.handle(10, request.clone()).code, Code::Ok);
    assert_eq!(
        manager
            .handle(
                10,
                Request::Stage {
                    digest: "0".repeat(64)
                }
            )
            .code,
        Code::ArtifactRejected
    );
    let binary = f
        .store
        .join("artifacts")
        .join(f.artifacts.digest())
        .join("bin/uob");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(&binary, b"altered-binary").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o555)).unwrap();
    assert_eq!(manager.handle(10, request).code, Code::ArtifactRejected);
    assert!(
        manager
            .handle(10, Request::Status {})
            .status
            .unwrap()
            .staged_verified_digest
            .is_none()
    );
}

#[test]
fn manager_packaging_has_an_independent_version_lifecycle_and_no_execution_surface() {
    let manifest = include_str!("../Cargo.toml");
    assert!(manifest.contains("version = \"0.1.0\""));
    let unit = include_str!("../../../packaging/systemd/uob-release-manager.service");
    for value in [
        "/usr/local/libexec/uob-release-manager serve",
        "StateDirectory=uob-release-manager",
        "RuntimeDirectoryMode=0750",
        "Restart=on-failure",
        "StartLimitBurst=5",
        "ProtectSystem=strict",
        "RestrictAddressFamilies=AF_UNIX",
        "CapabilityBoundingSet=\n",
    ] {
        assert!(unit.contains(value), "missing {value}");
    }
    for value in [
        "Requires=uob.service",
        "PartOf=uob.service",
        "BindsTo=uob.service",
        "ExecStart=/var/lib/uob-releases",
    ] {
        assert!(!unit.contains(value));
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_uob-release-manager"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("uob-release-manager {}", env!("CARGO_PKG_VERSION"))
    );
}
