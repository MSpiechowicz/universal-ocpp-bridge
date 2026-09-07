use std::collections::BTreeSet;

use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};

use super::{InstallError, reject};
use crate::{
    ArtifactCompatibilityManifest, FormatVersions, MigrationPolicy, SecurityCompatibilityPolicy,
};

pub const MANIFEST_LIMIT: usize = 64 * 1024;
pub const PAYLOAD_LIMIT: u64 = 512 * 1024 * 1024;

/// One regular file, encoded consecutively in the payload in manifest order.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleFile {
    pub path: String,
    pub bytes: u64,
}

/// Exact UTF-8 JSON bytes are signed with detached Ed25519; no reserialization.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleManifest {
    pub bundle_format: u32,
    pub release_id: String,
    pub source_commit: String,
    pub architecture: String,
    pub os_id: String,
    pub minimum_os_version: Vec<u32>,
    pub compatibility: ArtifactCompatibilityManifest,
    pub files: Vec<BundleFile>,
}

/// Administrator-provisioned trust and current host data; never supplied by a bundle.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallPolicy {
    pub trusted_ed25519_keys: Vec<Vec<u8>>,
    pub security: SecurityCompatibilityPolicy,
    pub current_formats: FormatVersions,
    pub architecture: String,
    pub os_id: String,
    pub os_version: Vec<u32>,
    /// Additional space allocated during installation for a consistent backup.
    pub backup_reserve_bytes: u64,
    /// Additional space allocated during installation for previous-release work.
    pub previous_reserve_bytes: u64,
}

pub(crate) fn digest_name(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn safe_path(path: &str) -> bool {
    if path == "bin/uob" {
        return true;
    }
    path.starts_with("assets/")
        && path.len() <= 240
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}

/// Authenticates metadata and checks host/schema/security eligibility before writing files.
///
/// # Errors
/// Rejects invalid signatures, format/path limits, revoked releases or incompatible hosts/data.
pub fn verify_manifest(
    encoded: &[u8],
    signature: &[u8],
    policy: &InstallPolicy,
) -> Result<BundleManifest, InstallError> {
    if encoded.len() > MANIFEST_LIMIT
        || signature.len() != 64
        || policy.trusted_ed25519_keys.is_empty()
        || policy.trusted_ed25519_keys.len() > 16
        || !policy.trusted_ed25519_keys.iter().any(|key| {
            key.len() == 32
                && UnparsedPublicKey::new(&ED25519, key)
                    .verify(encoded, signature)
                    .is_ok()
        })
    {
        return reject("missing or invalid trusted signature");
    }
    let manifest: BundleManifest = serde_json::from_slice(encoded)?;
    let c = &manifest.compatibility;
    if manifest.bundle_format != 1
        || !digest_name(c.artifact_digest.as_str())
        || manifest.release_id.is_empty()
        || manifest.release_id.len() > 128
        || !manifest
            .release_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || !matches!(manifest.source_commit.len(), 40 | 64)
        || !manifest
            .source_commit
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    {
        return reject("invalid bundle identity or format");
    }
    if manifest.architecture != policy.architecture
        || !matches!(manifest.architecture.as_str(), "x86_64" | "aarch64")
        || manifest.os_id != policy.os_id
        || policy.os_id.is_empty()
        || manifest.minimum_os_version.is_empty()
        || manifest.minimum_os_version.len() > 3
        || policy.os_version.is_empty()
        || policy.os_version.len() > 3
    {
        return reject("incompatible architecture or operating system");
    }
    let padded = |v: &[u32]| -> [u32; 3] {
        let mut result = [0; 3];
        result[..v.len()].copy_from_slice(v);
        result
    };
    if padded(&manifest.minimum_os_version) > padded(&policy.os_version) {
        return reject("operating system is below minimum version");
    }
    if c.release_sequence < policy.security.minimum_release_sequence
        || policy
            .security
            .revoked_artifacts
            .contains(&c.artifact_digest)
    {
        return reject("release is revoked or below security floor");
    }
    if c.local_migration != MigrationPolicy::AdditiveExpand
        || c.external_migration != MigrationPolicy::AdditiveExpand
    {
        return reject("non-additive migration");
    }
    let f = c.formats;
    let v = policy.current_formats;
    for (support, version) in [
        (f.public_contract, v.public_contract),
        (f.configuration, v.configuration),
        (f.operational_sqlite, v.operational_sqlite),
        (f.external_database, v.external_database),
    ] {
        if !support.readable.contains(version) || !support.writable.contains(version) {
            return reject("incompatible public, configuration or durable schema");
        }
    }
    if manifest.files.is_empty()
        || manifest.files.len() > 256
        || !manifest.files.iter().any(|f| f.path == "bin/uob")
    {
        return reject("bundle requires service and at most 256 files");
    }
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    for file in &manifest.files {
        if !safe_path(&file.path) || !seen.insert(&file.path) || file.bytes == 0 {
            return reject("invalid, duplicate or forbidden application path");
        }
        total = total
            .checked_add(file.bytes)
            .ok_or(InstallError::Rejected("size overflow"))?;
    }
    if total > PAYLOAD_LIMIT {
        return reject("bundle exceeds 512 MiB");
    }
    Ok(manifest)
}
