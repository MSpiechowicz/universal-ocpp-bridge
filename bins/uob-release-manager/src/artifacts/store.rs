use super::{
    BundleManifest, InstallError, InstallPolicy, filesystem as disk,
    manifest::{MANIFEST_LIMIT, digest_name},
    reject, verify_manifest,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, DirBuilder, File, Permissions},
    io::Read,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// A fully verified installation; this does not assert qualification or activation.
#[derive(Debug)]
pub struct InstalledArtifact {
    pub path: PathBuf,
    pub manifest: BundleManifest,
}

/// Exclusive owner of an administrator-created store. Holds a process-scoped OS lock.
pub struct ArtifactStore {
    root: PathBuf,
    _lock: File,
}

impl ArtifactStore {
    /// Opens a dedicated canonical directory; only one installer/activator may own it.
    ///
    /// # Errors
    /// Rejects unsafe paths, contention, malformed pointers and incomplete prior installs.
    pub fn open(root: &Path) -> Result<Self, InstallError> {
        Self::open_inner(root, false)
    }

    pub(crate) fn open_for_recovery(root: &Path) -> Result<Self, InstallError> {
        Self::open_inner(root, true)
    }

    fn open_inner(root: &Path, recovery: bool) -> Result<Self, InstallError> {
        disk::directory(root)?;
        // The same flock namespace as disk_preflight.py. Never replace the lock inode.
        let lock_path = root.join(".disk-admission.lock");
        let lock = match disk::open(&lock_path, true, true) {
            Ok(lock) => lock,
            Err(InstallError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                disk::open(&lock_path, true, false)?
            }
            Err(error) => return Err(error),
        };
        lock.try_lock()
            .map_err(|_| InstallError::Rejected("artifact store is busy"))?;
        let artifacts = root.join("artifacts");
        if !artifacts.try_exists()? {
            DirBuilder::new().mode(0o755).create(&artifacts)?;
        }
        disk::directory(&artifacts)?;
        let store = Self {
            root: root.to_owned(),
            _lock: lock,
        };
        store.references()?;
        // An interrupted private extraction requires explicit operator inspection.
        // It is never considered an installed artifact or an activation candidate.
        if store.root.join(".incoming").try_exists()?
            || store.root.join(".candidate-next").try_exists()?
        {
            return reject(
                "incomplete installation; inspect private temporary state before retrying",
            );
        }
        if !recovery
            && (fs::symlink_metadata(root.join(".activation-intent")).is_ok()
                || fs::symlink_metadata(root.join(".activation-intent.next")).is_ok())
        {
            return reject("activation recovery required before artifact access");
        }
        if !recovery {
            crate::activation::check_store_consistency(root)?;
            store.prune()?;
        }
        Ok(store)
    }

    fn references(&self) -> Result<BTreeSet<String>, InstallError> {
        let mut result = BTreeSet::new();
        for name in ["active", "previous-good", "candidate"] {
            let path = self.root.join(name);
            // symlink_metadata deliberately distinguishes dangling symlinks from absence.
            match fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
                Ok(_) => (),
            }
            let value = String::from_utf8(disk::bounded_read(&path, 64)?)
                .map_err(|_| InstallError::Rejected("invalid artifact pointer"))?;
            if !digest_name(&value) {
                return reject("invalid artifact pointer");
            }
            disk::directory(&self.root.join("artifacts").join(&value))?;
            result.insert(value);
        }
        Ok(result)
    }

    fn prune(&self) -> Result<(), InstallError> {
        let retained = self.references()?;
        for entry in fs::read_dir(self.root.join("artifacts"))? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| InstallError::Rejected("unexpected artifact name"))?;
            if !digest_name(&name) {
                return reject("unexpected artifact name");
            }
            if !retained.contains(&name) {
                disk::remove_tree(&entry.path())?;
            }
        }
        disk::sync_dir(&self.root.join("artifacts"))
    }

    /// Installs an authenticated manifest plus exact concatenated file bytes.
    ///
    /// # Errors
    /// Rejects bad signatures/content, unsafe entries, disk exhaustion and immutable collisions.
    /// Active and previous-good pointers and contents are never changed.
    pub fn install(
        &self,
        encoded: &[u8],
        signature: &[u8],
        payload: &mut impl Read,
        policy: &InstallPolicy,
    ) -> Result<InstalledArtifact, InstallError> {
        if crate::activation::installation_blocked(&self.root)? {
            return reject("activation or probation owns the candidate slot");
        }
        let manifest = verify_manifest(encoded, signature, policy)?;
        self.references()?;
        let destination = self
            .root
            .join("artifacts")
            .join(manifest.compatibility.artifact_digest.as_str());
        if fs::symlink_metadata(&destination).is_ok() {
            return reject("artifact already exists; immutable overwrite refused");
        }
        let total: u64 = manifest.files.iter().map(|f| f.bytes).sum();
        let reserved = policy
            .backup_reserve_bytes
            .checked_add(policy.previous_reserve_bytes)
            .filter(|sum| *sum <= 1024 * 1024 * 1024 * 1024)
            .ok_or(InstallError::Rejected("invalid disk reservation"))?;
        if policy.backup_reserve_bytes == 0 || policy.previous_reserve_bytes == 0 {
            return reject("backup and previous-release reservations are required");
        }
        disk::capacity(
            &self.root,
            total + reserved + MANIFEST_LIMIT as u64,
            manifest.files.len() as u64 * 4 + 16,
        )?;
        let temporary = self.root.join(".incoming");
        DirBuilder::new().mode(0o700).create(&temporary)?;
        let result = self.extract(&temporary, encoded, signature, payload, &manifest, reserved);
        if let Err(error) = result {
            disk::remove_tree(&temporary)?;
            return Err(error);
        }
        // Moving a directory between parents requires owner write permission for `..`.
        // No reader can select it until the candidate pointer is published below.
        fs::set_permissions(&temporary, Permissions::from_mode(0o755))?;
        fs::rename(&temporary, &destination)?;
        fs::set_permissions(&destination, Permissions::from_mode(0o555))?;
        disk::sync_dir(&destination)?;
        disk::sync_dir(&self.root.join("artifacts"))?;
        disk::sync_dir(&self.root)?;
        disk::write_new(
            &self.root.join(".candidate-next"),
            manifest.compatibility.artifact_digest.as_str().as_bytes(),
        )?;
        fs::rename(
            self.root.join(".candidate-next"),
            self.root.join("candidate"),
        )?;
        disk::sync_dir(&self.root)?;
        self.prune()?;
        Ok(InstalledArtifact {
            path: destination,
            manifest,
        })
    }

    fn extract(
        &self,
        temporary: &Path,
        encoded: &[u8],
        signature: &[u8],
        payload: &mut impl Read,
        manifest: &BundleManifest,
        reserve: u64,
    ) -> Result<(), InstallError> {
        let reserve_file = disk::open(&temporary.join(".capacity"), true, true)?;
        disk::allocate(&reserve_file, reserve)?;
        let mut files = Vec::new();
        for entry in &manifest.files {
            let path = temporary.join(&entry.path);
            let parent = path.parent().expect("validated file parent");
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
            let file = disk::open(&path, true, true)?;
            disk::allocate(&file, entry.bytes)?;
            files.push((file, path, entry));
        }
        disk::capacity(&self.root, MANIFEST_LIMIT as u64, 16)?;
        let mut digest = Sha256::new();
        for (mut file, path, entry) in files {
            disk::copy_exact(payload, &mut file, entry.bytes, &mut digest)?;
            let mode = if entry.path == "bin/uob" {
                0o555
            } else {
                0o444
            };
            fs::set_permissions(path, Permissions::from_mode(mode))?;
            file.sync_all()?;
        }
        let mut extra = [0];
        if payload.read(&mut extra)? != 0 {
            return reject("trailing bundle bytes");
        }
        if disk::hex(&digest.finalize()) != manifest.compatibility.artifact_digest.as_str() {
            return reject("bundle content digest mismatch");
        }
        disk::write_new(&temporary.join("manifest.json"), encoded)?;
        disk::write_new(&temporary.join("manifest.sig"), signature)?;
        for name in ["manifest.json", "manifest.sig"] {
            fs::set_permissions(temporary.join(name), Permissions::from_mode(0o444))?;
            disk::open(&temporary.join(name), false, false)?.sync_all()?;
        }
        // Space was actually allocated alongside extraction, not merely observed.
        drop(reserve_file);
        fs::remove_file(temporary.join(".capacity"))?;
        disk::seal(temporary)
    }

    /// Revalidates a retained artifact against current trust, revocations and host policy.
    /// Call immediately before staging/activation, then apply `NormalPromotionGate` evidence.
    ///
    /// # Errors
    /// Rejects any changed bytes, signature, eligibility, link, or missing file.
    pub fn verify_installed(
        &self,
        digest: &str,
        policy: &InstallPolicy,
    ) -> Result<InstalledArtifact, InstallError> {
        if !digest_name(digest) {
            return reject("invalid artifact digest");
        }
        let path = self.root.join("artifacts").join(digest);
        disk::directory(&path)?;
        let encoded = disk::bounded_read(&path.join("manifest.json"), MANIFEST_LIMIT)?;
        let signature = disk::bounded_read(&path.join("manifest.sig"), 64)?;
        let manifest = verify_manifest(&encoded, &signature, policy)?;
        if manifest.compatibility.artifact_digest.as_str() != digest {
            return reject("manifest identity mismatch");
        }
        disk::capacity(&self.root, 0, 16)?;
        let mut expected: BTreeSet<String> =
            manifest.files.iter().map(|f| f.path.clone()).collect();
        expected.extend(["manifest.json".to_owned(), "manifest.sig".to_owned()]);
        disk::verify_tree(&path, &path, &expected)?;
        let mut hasher = Sha256::new();
        for entry in &manifest.files {
            let file_path = path.join(&entry.path);
            // Check each ancestor too, so replacing an assets directory with a link fails.
            let mut parent = file_path.parent();
            while let Some(directory) = parent {
                disk::directory(directory)?;
                if directory == path {
                    break;
                }
                parent = directory.parent();
            }
            let file = disk::open(&file_path, false, false)?;
            if file.metadata()?.len() != entry.bytes {
                return reject("installed file size mismatch");
            }
            let mut file = file.take(entry.bytes + 1);
            let mut buffer = vec![0; 64 * 1024];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
        }
        if disk::hex(&hasher.finalize()) != digest {
            return reject("installed content digest mismatch");
        }
        Ok(InstalledArtifact { path, manifest })
    }
}
