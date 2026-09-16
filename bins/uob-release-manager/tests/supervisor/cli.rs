use super::support::{Daemon, Fixture};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};
use uob_release_manager::supervisor::Permission;

fn invoke(daemon: &Daemon, arguments: &[&str], success: bool) -> Vec<Value> {
    let binary = std::env::var_os("UOB_TEST_CLI").expect("run scripts/test-release-cli.sh");
    let output = Command::new(binary)
        .arg("release")
        .args(arguments)
        .arg("--socket")
        .arg(&daemon.socket)
        // No service configuration, management listener, or bridge process exists.
        .current_dir(daemon.socket.parent().unwrap())
        .output()
        .unwrap();
    assert_eq!(
        output.status.success(),
        success,
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if success {
        assert!(output.stderr.is_empty());
    }
    assert!(!String::from_utf8_lossy(&output.stderr).contains("do-not-trust-this-secret"));
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
#[ignore = "requires separately built uob; run scripts/test-release-cli.sh"]
fn commands_remain_available_without_bridge_and_preserve_policy_on_restart() {
    let f = Fixture::new();
    let bundle = f.store.join("artifacts").join(f.artifacts.digest());
    let evidence = f.artifacts.root.join("untrusted-evidence.json");
    fs::write(&evidence, br#"{"claims":"do-not-trust-this-secret"}"#).unwrap();
    {
        let daemon = f.launch(&[Permission::Read, Permission::Stage, Permission::Activate]);
        assert_eq!(
            invoke(&daemon, &["stage", "--bundle", text(&bundle)], true)[0]["code"],
            "ok"
        );
        assert_eq!(
            invoke(
                &daemon,
                &[
                    "qualify",
                    "--release",
                    f.artifacts.digest(),
                    "--evidence",
                    text(&evidence)
                ],
                false,
            )[0]["code"],
            "evidence_rejected"
        );
        assert_eq!(
            invoke(
                &daemon,
                &["promote", "--release", f.artifacts.digest()],
                false
            )[0]["code"],
            "qualification_required"
        );
        assert_eq!(
            invoke(&daemon, &["rollback", "--to", "previous-good"], false)[0]["code"],
            "qualification_required"
        );
        let status = invoke(&daemon, &["status", "--format", "json"], true);
        assert_eq!(status[0]["status"]["sequence"], 4);
        assert_eq!(status[0]["status"]["failed_operations"], 3);
        assert_eq!(
            status[0]["status"]["staged_verified_digest"],
            f.artifacts.digest()
        );
        assert!(!f.store.join("active").exists());
        assert!(!f.store.join("previous-good").exists());
    }
    let daemon = f.launch(&[Permission::Read]);
    let events = invoke(&daemon, &["events", "--format", "jsonl"], true);
    let records: Vec<_> = events
        .iter()
        .filter(|value| value.get("request").is_some())
        .collect();
    assert_eq!(records.len(), 4);
    for (index, record) in records.iter().enumerate() {
        assert_eq!(record["sequence"], index + 1);
        assert_eq!(record["uid"], rustix::process::geteuid().as_raw());
    }
    assert_eq!(records[0]["request"]["operation"], "stage");
    assert_eq!(records[1]["request"]["operation"], "qualify");
    assert_eq!(records[2]["request"]["operation"], "promote");
    assert_eq!(records[3]["request"]["operation"], "rollback");
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("do-not-trust-this-secret")
    );
    let page = invoke(
        &daemon,
        &["events", "--format", "jsonl", "--after", "3"],
        true,
    );
    let records: Vec<_> = page
        .iter()
        .filter(|value| value.get("request").is_some())
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["sequence"], 4);

    for arguments in [
        vec!["stage", "--bundle", text(&bundle)],
        vec![
            "qualify",
            "--release",
            f.artifacts.digest(),
            "--evidence",
            text(&evidence),
        ],
        vec!["promote", "--release", f.artifacts.digest()],
        vec!["rollback", "--to", "previous-good"],
    ] {
        assert_eq!(invoke(&daemon, &arguments, false)[0]["code"], "forbidden");
    }
    let status = invoke(&daemon, &["status", "--format", "json"], true);
    assert_eq!(status[0]["status"]["sequence"], 4);
}

#[test]
#[ignore = "requires separately built uob; run scripts/test-release-cli.sh"]
fn stage_permission_does_not_grant_read_access() {
    let f = Fixture::new();
    let daemon = f.launch(&[Permission::Stage]);
    assert_eq!(
        invoke(&daemon, &["status", "--format", "json"], false)[0]["code"],
        "forbidden"
    );
    assert_eq!(
        invoke(&daemon, &["events", "--format", "jsonl"], false)[0]["code"],
        "forbidden"
    );
}

#[test]
#[ignore = "requires separately built uob; run scripts/test-release-cli.sh"]
fn unsigned_bundle_selection_cannot_override_supervisor_candidate() {
    let f = Fixture::new();
    let daemon = f.launch(&[Permission::Read, Permission::Stage]);
    let manifest = json!({"compatibility": {"artifact_digest": "0".repeat(64)}});
    fs::write(
        f.artifacts.root.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let response = invoke(
        &daemon,
        &["stage", "--bundle", text(&f.artifacts.root)],
        false,
    );
    assert_eq!(response[0]["code"], "artifact_rejected");
    assert_eq!(
        fs::read_to_string(f.store.join("candidate")).unwrap(),
        f.artifacts.digest()
    );
    let status = invoke(&daemon, &["status", "--format", "json"], true);
    assert!(status[0]["status"]["staged_verified_digest"].is_null());
}
