use super::support::Fixture;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};
use uob_release_manager::{
    SchemaVersion,
    artifacts::{ArtifactStore, BundleFile, verify_manifest},
};

#[test]
fn signature_and_host_eligibility_fail_closed() {
    let mut f = Fixture::new();
    let (encoded, signature) = f.signed();
    assert!(verify_manifest(&encoded, &[], &f.policy).is_err());
    assert!(verify_manifest(&encoded, &[0; 64], &f.policy).is_err());
    let mut changed = encoded.clone();
    changed.push(b' ');
    assert!(verify_manifest(&changed, &signature, &f.policy).is_err());
    let original = f.policy.clone();
    f.policy.architecture = "x86_64".into();
    assert!(verify_manifest(&encoded, &signature, &f.policy).is_err());
    f.policy = original.clone();
    f.policy.os_version = vec![11];
    assert!(verify_manifest(&encoded, &signature, &f.policy).is_err());
    f.policy = original.clone();
    f.policy.os_id = "ubuntu".into();
    assert!(verify_manifest(&encoded, &signature, &f.policy).is_err());
    f.policy = original.clone();
    f.policy.current_formats.operational_sqlite = SchemaVersion::new(2);
    assert!(verify_manifest(&encoded, &signature, &f.policy).is_err());
    f.policy = original.clone();
    f.policy.security.minimum_release_sequence = 13;
    assert!(verify_manifest(&encoded, &signature, &f.policy).is_err());
    f.policy = original;
    f.policy.trusted_ed25519_keys = vec![vec![0; 32]];
    assert!(verify_manifest(&encoded, &signature, &f.policy).is_err());
}

#[test]
fn signed_paths_cannot_escape_or_replace_supervisor_or_install_links() {
    for path in [
        "../outside",
        "/bin/uob",
        "assets/../../escape",
        "assets//x",
        "assets/./x",
        "bin/uob-release-manager",
        "uob-release-manager",
        "assets/x\\y",
        "assets/",
        "bin/uob",
    ] {
        let mut f = Fixture::new();
        f.manifest.files[1].path = path.into();
        let (encoded, signature) = f.signed();
        let store = ArtifactStore::open(&f.root).unwrap();
        assert!(
            store
                .install(&encoded, &signature, &mut f.payload.as_slice(), &f.policy)
                .is_err(),
            "{path}"
        );
        assert!(!f.root.join("candidate").exists());
        assert!(!f.root.join(".incoming").exists());
    }
    let f = Fixture::new();
    let (encoded, _) = f.signed();
    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    value["files"][0]["type"] = "symlink".into();
    let encoded = serde_json::to_vec(&value).unwrap();
    assert!(verify_manifest(&encoded, f.key.sign(&encoded).as_ref(), &f.policy).is_err());
}

#[test]
fn file_directory_collisions_are_cleaned_without_publishing() {
    let mut f = Fixture::new();
    f.manifest.files.push(BundleFile {
        path: "assets/index.html/child".into(),
        bytes: 1,
    });
    let (encoded, signature) = f.signed();
    let store = ArtifactStore::open(&f.root).unwrap();
    assert!(
        store
            .install(&encoded, &signature, &mut f.payload.as_slice(), &f.policy)
            .is_err()
    );
    assert!(!f.root.join(".incoming").exists());
    assert!(!f.root.join("candidate").exists());
}

#[test]
fn insufficient_capacity_and_size_bounds_do_not_create_candidate() {
    let mut f = Fixture::new();
    let store = ArtifactStore::open(&f.root).unwrap();
    f.policy.backup_reserve_bytes = 1024 * 1024 * 1024 * 1024 - f.policy.previous_reserve_bytes;
    let (encoded, signature) = f.signed();
    assert!(
        store
            .install(&encoded, &signature, &mut f.payload.as_slice(), &f.policy)
            .is_err()
    );
    assert!(!f.root.join(".incoming").exists());
    f.manifest.files[0].bytes = u64::MAX;
    let (encoded, signature) = f.signed();
    assert!(verify_manifest(&encoded, &signature, &f.policy).is_err());
}

#[test]
fn locks_links_and_interrupted_extractions_fail_closed() {
    let f = Fixture::new();
    let store = ArtifactStore::open(&f.root).unwrap();
    assert!(ArtifactStore::open(&f.root).is_err());
    drop(store);
    fs::create_dir(f.root.join(".incoming")).unwrap();
    assert!(ArtifactStore::open(&f.root).is_err());
    fs::remove_dir(f.root.join(".incoming")).unwrap();
    symlink("missing", f.root.join("active")).unwrap();
    assert!(ArtifactStore::open(&f.root).is_err());
    fs::remove_file(f.root.join("active")).unwrap();
    fs::set_permissions(&f.root, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(ArtifactStore::open(&f.root).is_err());
}

#[test]
fn installed_symlink_and_hardlink_substitution_is_rejected() {
    let f = Fixture::new();
    let store = ArtifactStore::open(&f.root).unwrap();
    let (encoded, signature) = f.signed();
    let installed = store
        .install(&encoded, &signature, &mut f.payload.as_slice(), &f.policy)
        .unwrap();
    let assets = installed.path.join("assets");
    fs::set_permissions(&assets, fs::Permissions::from_mode(0o700)).unwrap();
    let index = assets.join("index.html");
    fs::remove_file(&index).unwrap();
    symlink("../bin/uob", &index).unwrap();
    assert!(store.verify_installed(f.digest(), &f.policy).is_err());
    fs::remove_file(&index).unwrap();
    fs::hard_link(installed.path.join("bin/uob"), &index).unwrap();
    assert!(store.verify_installed(f.digest(), &f.policy).is_err());
}

#[test]
fn unsigned_extra_assets_and_changed_signature_are_not_eligible() {
    let f = Fixture::new();
    let store = ArtifactStore::open(&f.root).unwrap();
    let (encoded, signature) = f.signed();
    let installed = store
        .install(&encoded, &signature, &mut f.payload.as_slice(), &f.policy)
        .unwrap();
    let assets = installed.path.join("assets");
    fs::set_permissions(&assets, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(assets.join("injected.js"), "malicious").unwrap();
    fs::set_permissions(&assets, fs::Permissions::from_mode(0o555)).unwrap();
    assert!(store.verify_installed(f.digest(), &f.policy).is_err());
    fs::set_permissions(&assets, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_file(assets.join("injected.js")).unwrap();
    fs::set_permissions(&assets, fs::Permissions::from_mode(0o555)).unwrap();
    store.verify_installed(f.digest(), &f.policy).unwrap();
    let signature = installed.path.join("manifest.sig");
    fs::set_permissions(&signature, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&signature, [0; 64]).unwrap();
    fs::set_permissions(&signature, fs::Permissions::from_mode(0o444)).unwrap();
    assert!(store.verify_installed(f.digest(), &f.policy).is_err());
}
