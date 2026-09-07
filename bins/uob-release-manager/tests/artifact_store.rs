#[path = "artifact_store/adversarial.rs"]
mod adversarial;
#[path = "artifact_store/support.rs"]
mod support;
use std::{fs, os::unix::fs::PermissionsExt};
use support::Fixture;
use uob_release_manager::artifacts::ArtifactStore;

#[test]
fn signed_install_and_reverification_preserve_exact_bytes_and_permissions() {
    let f = Fixture::new();
    let store = ArtifactStore::open(&f.root).unwrap();
    let (encoded, signature) = f.signed();
    let installed = store
        .install(&encoded, &signature, &mut f.payload.as_slice(), &f.policy)
        .unwrap();
    assert_eq!(
        fs::read(installed.path.join("bin/uob")).unwrap(),
        &f.payload[..14]
    );
    assert_eq!(
        fs::read(installed.path.join("manifest.json")).unwrap(),
        encoded
    );
    assert_eq!(
        fs::metadata(installed.path.join("bin/uob"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o555
    );
    assert_eq!(
        fs::metadata(&installed.path).unwrap().permissions().mode() & 0o777,
        0o555
    );
    assert_eq!(
        fs::read_to_string(f.root.join("candidate")).unwrap(),
        f.digest()
    );
    store.verify_installed(f.digest(), &f.policy).unwrap();
    assert!(!f.root.join("active").exists());
    assert!(!f.root.join(".incoming").exists());
    assert!(
        store
            .install(&encoded, &signature, &mut f.payload.as_slice(), &f.policy)
            .is_err()
    );
}

#[test]
fn replacing_candidate_retains_active_and_previous_good_across_reopen() {
    let mut f = Fixture::new();
    let mut retained = Vec::new();
    for role in ["previous-good", "active", "candidate", "candidate"] {
        let store = ArtifactStore::open(&f.root).unwrap();
        let (encoded, signature) = f.signed();
        store
            .install(&encoded, &signature, &mut f.payload.as_slice(), &f.policy)
            .unwrap();
        if role != "candidate" {
            fs::write(f.root.join(role), f.digest()).unwrap();
            retained.push(f.digest().to_owned());
        }
        f.next_payload();
    }
    let store = ArtifactStore::open(&f.root).unwrap();
    assert_eq!(fs::read_dir(f.root.join("artifacts")).unwrap().count(), 3);
    for digest in retained {
        store.verify_installed(&digest, &f.policy).unwrap();
    }
}

#[test]
fn malformed_payload_never_replaces_existing_candidate_or_active_state() {
    let mut f = Fixture::new();
    let store = ArtifactStore::open(&f.root).unwrap();
    let (encoded, signature) = f.signed();
    let installed = store
        .install(&encoded, &signature, &mut f.payload.as_slice(), &f.policy)
        .unwrap();
    fs::write(f.root.join("active"), f.digest()).unwrap();
    let old_digest = f.digest().to_owned();
    f.next_payload();
    let (encoded, signature) = f.signed();
    for payload in [
        f.payload[..5].to_vec(),
        [f.payload.clone(), vec![0]].concat(),
        vec![0; f.payload.len()],
    ] {
        assert!(
            store
                .install(&encoded, &signature, &mut payload.as_slice(), &f.policy)
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(f.root.join("candidate")).unwrap(),
            old_digest
        );
        assert_eq!(
            fs::read_to_string(f.root.join("active")).unwrap(),
            old_digest
        );
        assert!(installed.path.exists());
        assert!(!f.root.join(".incoming").exists());
    }
}

#[test]
fn revalidation_detects_corruption_and_new_revocation() {
    let mut f = Fixture::new();
    let store = ArtifactStore::open(&f.root).unwrap();
    let (encoded, signature) = f.signed();
    let installed = store
        .install(&encoded, &signature, &mut f.payload.as_slice(), &f.policy)
        .unwrap();
    f.policy
        .security
        .revoked_artifacts
        .insert(f.manifest.compatibility.artifact_digest.clone());
    assert!(store.verify_installed(f.digest(), &f.policy).is_err());
    f.policy.security.revoked_artifacts.clear();
    let file = installed.path.join("bin/uob");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&file, vec![0; 14]).unwrap();
    assert!(store.verify_installed(f.digest(), &f.policy).is_err());
}
