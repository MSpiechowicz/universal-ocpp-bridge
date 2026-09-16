use super::*;
use uob_release_manager::supervisor::{
    Decision,
    audit::{Compatibility, Drain, Health},
    rollback,
};

#[test]
fn stage_qualification_promotion_and_automatic_rollback_leave_an_ordered_durable_audit() {
    exercise_audit(false);
}

#[test]
#[ignore = "requires separately built uob; run scripts/test-release-cli.sh"]
fn cli_reads_decisions_after_rollback_and_bridge_crash() {
    exercise_audit(true);
}

fn exercise_audit(check_cli: bool) {
    let _serial = SERIAL.lock().unwrap();
    run(async {
        let previous_binary = [BINARY, b"\n# previous release\n"].concat();
        let f = Fixture::with_binaries(Some(&previous_binary), Some(BINARY));
        let p = policy(&f);
        let store = Arc::new(Store::open(&p.operational_database, 16).unwrap());
        seed(&store).await;
        let mut process = Process::new(&f, store.clone());
        process.boot_old().await;
        let mut manager = f.manager().with_preflight_policy(p.clone()).unwrap();
        assert_eq!(
            manager
                .handle(
                    100,
                    Request::Stage {
                        digest: f.artifact.digest().into()
                    }
                )
                .code,
            Code::Ok
        );
        let qualification = f.publish();
        assert_eq!(manager.handle(100, qualification.clone()).code, Code::Ok);
        assert_eq!(
            manager
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
                .await
                .code,
            Code::Ok
        );
        let mut staging = StopStaging;
        for (id, signal) in [
            (1, Signal::Started { invocation: 1 }),
            (
                2,
                Signal::FatalInvariant {
                    data_valid_and_compatible: true,
                },
            ),
        ] {
            assert_eq!(
                manager
                    .observe_failure_and_rollback(
                        FailurePolicy::default(),
                        Observation {
                            id,
                            at_seconds: id,
                            signal,
                            resource_pressure: false
                        },
                        &mut staging,
                        &mut process,
                    )
                    .await
                    .unwrap(),
                Code::Ok
            );
        }
        let before = manager
            .handle(100, Request::Events { after: 0 })
            .events
            .unwrap();
        assert_decisions(&f, &before, &qualification);
        assert_eq!(manager.handle(100, Request::Status {}).code, Code::Ok);
        drop(manager);
        let mut manager = f.manager().with_preflight_policy(p).unwrap();
        let after = manager
            .handle(100, Request::Events { after: 0 })
            .events
            .unwrap();
        assert_eq!(after.records, before.records);
        assert_eq!(after.records.last().unwrap().request, Request::Rollback {});
        process
            .store
            .shutdown(Duration::from_secs(1))
            .await
            .unwrap();
        if check_cli {
            drop(manager);
            let mut bridge = process.child.take().unwrap();
            bridge.kill().unwrap();
            bridge.wait().unwrap();
            cli_after_crash(&f);
        }
    });
}

fn assert_decisions(
    f: &Fixture,
    before: &uob_release_manager::supervisor::Events,
    qualification: &Request,
) {
    assert!(
        before
            .records
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    let operators: Vec<_> = before
        .records
        .iter()
        .filter(|record| record.actor == Actor::Operator)
        .collect();
    assert_eq!(operators.len(), 3);
    assert!(
        operators
            .iter()
            .all(|record| record.uid == 100 && record.result == Code::Ok)
    );
    assert_eq!(
        operators[0].request,
        Request::Stage {
            digest: f.artifact.digest().into()
        }
    );
    assert_eq!(&operators[1].request, qualification);
    assert_eq!(
        operators[2].request,
        Request::Promote {
            digest: f.artifact.digest().into()
        }
    );
    let Request::Qualify {
        evidence_digest, ..
    } = qualification
    else {
        panic!("qualification fixture must supply evidence");
    };
    assert!(
        before
            .records
            .iter()
            .any(|record| matches!(&record.decision,
                Some(Decision::Promote {
                    candidate_digest,
                    previous_good_digest: Some(previous),
                    evidence_digest: Some(evidence),
                    compatibility: Compatibility::Accepted,
                    drain: Drain::Granted,
                    health: Health::Probation,
                    ..
                }) if candidate_digest == f.artifact.digest()
                    && previous == f.previous.compatibility.artifact_digest.as_str()
                    && evidence == evidence_digest
            ))
    );
    let restored = before.records.last().unwrap();
    assert_eq!(restored.actor, Actor::Supervisor);
    assert_eq!(restored.uid, rustix::process::geteuid().as_raw());
    assert!(matches!(&restored.decision,
        Some(Decision::Rollback {
            quarantined_digest: Some(candidate),
            previous_good_digest: Some(previous),
            step: rollback::Step::Restored,
            ..
        }) if candidate == f.artifact.digest()
            && previous == f.previous.compatibility.artifact_digest.as_str()
    ));
}

#[test]
#[ignore = "subprocess release IPC fixture"]
fn cli_daemon() {
    let input = std::env::var_os("UOB_AUDIT_DAEMON").expect("subprocess fixture input");
    let input: serde_json::Value = serde_json::from_slice(&fs::read(input).unwrap()).unwrap();
    let manager = uob_release_manager::supervisor::Supervisor::open(
        std::path::Path::new(input["state"].as_str().unwrap()),
        std::path::Path::new(input["store"].as_str().unwrap()),
        serde_json::from_value(input["policy"].clone()).unwrap(),
        vec![uob_release_manager::supervisor::Grant {
            uid: rustix::process::geteuid().as_raw(),
            permissions: vec![uob_release_manager::supervisor::Permission::Read],
        }],
    )
    .unwrap();
    uob_release_manager::supervisor::ipc::Server::bind(std::path::Path::new(
        input["runtime"].as_str().unwrap(),
    ))
    .unwrap()
    .run(manager)
    .unwrap();
}

struct Daemon(std::process::Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn cli_after_crash(f: &Fixture) {
    use std::{
        os::unix::fs::PermissionsExt,
        process::{Command, Stdio},
        time::Instant,
    };
    let runtime = f.artifact.root.join("audit-runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let input = f.artifact.root.join("audit-daemon.json");
    fs::write(
        &input,
        serde_json::to_vec(&serde_json::json!({
            "state": f.state, "store": f.store, "runtime": runtime, "policy": f.artifact.policy
        }))
        .unwrap(),
    )
    .unwrap();
    let mut daemon = Daemon(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "audit::cli_daemon", "--ignored"])
            .env("UOB_AUDIT_DAEMON", input)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let socket = runtime.join("control.sock");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(daemon.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = Command::new(std::env::var_os("UOB_TEST_CLI").unwrap())
        .args(["release", "events", "--format", "jsonl", "--socket"])
        .arg(socket)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let values: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let restored = values
        .iter()
        .find(|value| value["decision"]["step"] == "restored")
        .unwrap();
    assert_eq!(restored["actor"], "supervisor");
    assert_eq!(
        restored["decision"]["quarantined_digest"],
        f.artifact.digest()
    );
    assert_eq!(
        restored["decision"]["previous_good_digest"],
        f.previous.compatibility.artifact_digest.as_str()
    );
    assert_eq!(values.last().unwrap()["truncated"], false);
}
