#[allow(dead_code)]
#[path = "artifact_store/support.rs"]
mod support;
use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};
use support::Fixture;
use uob_release_manager::{
    activation::ActivationJournal,
    artifacts::{ArtifactStore, InstallPolicy},
};

#[test]
#[ignore = "subprocess fixture for activation ownership"]
fn owner_process() {
    let Some(root) = std::env::var_os("UOB_ACTIVATION_TEST_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let policy: InstallPolicy =
        serde_json::from_slice(&fs::read(root.join("test-policy.json")).unwrap()).unwrap();
    let _journal = ActivationJournal::open(&root, &policy).unwrap();
    println!("owned");
    // Parent kills the process to verify kernel lock release, without graceful Drop.
    loop {
        std::thread::park();
    }
}

#[test]
fn killed_owner_releases_kernel_lock_and_preserves_persistent_state() {
    let f = Fixture::new();
    let (bytes, signature) = f.signed();
    ArtifactStore::open(&f.root)
        .unwrap()
        .install(&bytes, &signature, &mut f.payload.as_slice(), &f.policy)
        .unwrap();
    fs::write(
        f.root.join("test-policy.json"),
        serde_json::to_vec(&f.policy).unwrap(),
    )
    .unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "owner_process", "--ignored", "--nocapture"])
        .env("UOB_ACTIVATION_TEST_ROOT", &f.root)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let output = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            if line == "owned" {
                let _ = sender.send(());
                break;
            }
        }
    });
    if receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .is_err()
    {
        let _ = child.kill();
        let _ = child.wait();
        panic!("owner did not become ready");
    }
    let blocked = ActivationJournal::open(&f.root, &f.policy).is_err();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(blocked);
    let journal = ActivationJournal::open(&f.root, &f.policy).unwrap();
    assert_eq!(
        journal.state().candidate.as_ref().unwrap().digest,
        f.digest()
    );
    assert!(ActivationJournal::open(&f.root, &f.policy).is_err());
    assert!(journal.state().production.is_none());
}
