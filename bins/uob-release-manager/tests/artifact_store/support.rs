use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use uob_contracts::ArtifactDigest;
use uob_release_manager::{
    ArtifactCompatibilityManifest, FormatCompatibility, FormatSupport, FormatVersions,
    MigrationPolicy, SchemaRange, SchemaVersion, SecurityCompatibilityPolicy,
    artifacts::{BundleFile, BundleManifest, InstallPolicy},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
pub struct Fixture {
    pub root: PathBuf,
    pub key: Ed25519KeyPair,
    pub manifest: BundleManifest,
    pub policy: InstallPolicy,
    pub payload: Vec<u8>,
}
impl Fixture {
    pub fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "uob-artifacts-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let key = Ed25519KeyPair::from_seed_unchecked(&[19; 32]).unwrap();
        let v = SchemaVersion::new(1);
        let range = SchemaRange::new(v, v).unwrap();
        let support = FormatSupport {
            readable: range,
            writable: range,
        };
        let payload = b"service-binary<html>console</html>".to_vec();
        let digest = ArtifactDigest::new(hash(&payload)).unwrap();
        let manifest = BundleManifest {
            bundle_format: 1,
            release_id: "candidate-1".into(),
            source_commit: "a".repeat(40),
            architecture: "aarch64".into(),
            os_id: "debian".into(),
            minimum_os_version: vec![12],
            compatibility: ArtifactCompatibilityManifest {
                artifact_digest: digest,
                release_sequence: 12,
                formats: FormatCompatibility {
                    public_contract: support,
                    configuration: support,
                    operational_sqlite: support,
                    external_database: support,
                },
                local_migration: MigrationPolicy::AdditiveExpand,
                external_migration: MigrationPolicy::AdditiveExpand,
            },
            files: vec![
                BundleFile {
                    path: "bin/uob".into(),
                    bytes: 14,
                },
                BundleFile {
                    path: "assets/index.html".into(),
                    bytes: payload.len() as u64 - 14,
                },
            ],
        };
        let policy = InstallPolicy {
            trusted_ed25519_keys: vec![key.public_key().as_ref().to_vec()],
            security: SecurityCompatibilityPolicy {
                minimum_release_sequence: 10,
                revoked_artifacts: BTreeSet::new(),
            },
            current_formats: FormatVersions {
                public_contract: v,
                configuration: v,
                operational_sqlite: v,
                external_database: v,
            },
            architecture: "aarch64".into(),
            os_id: "debian".into(),
            os_version: vec![12],
            backup_reserve_bytes: 4096,
            previous_reserve_bytes: 4096,
        };
        Self {
            root,
            key,
            manifest,
            policy,
            payload,
        }
    }
    pub fn signed(&self) -> (Vec<u8>, Vec<u8>) {
        let encoded = serde_json::to_vec(&self.manifest).unwrap();
        let signature = self.key.sign(&encoded).as_ref().to_vec();
        (encoded, signature)
    }
    pub fn digest(&self) -> &str {
        self.manifest.compatibility.artifact_digest.as_str()
    }
    pub fn next_payload(&mut self) {
        self.payload[0] = self.payload[0].wrapping_add(1);
        self.manifest.compatibility.artifact_digest =
            ArtifactDigest::new(hash(&self.payload)).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fn writable(path: &std::path::Path) {
            if fs::symlink_metadata(path).unwrap().is_dir() {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
                for child in fs::read_dir(path).unwrap() {
                    writable(&child.unwrap().path());
                }
            }
        }
        writable(&self.root);
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn hash(bytes: &[u8]) -> String {
    use std::fmt::Write;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, b| {
            write!(out, "{b:02x}").unwrap();
            out
        })
}
