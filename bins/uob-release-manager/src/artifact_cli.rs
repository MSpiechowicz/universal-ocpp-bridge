use std::{
    error::Error,
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};
use uob_release_manager::artifacts::{ArtifactStore, InstallPolicy};

pub(crate) fn read(path: &Path, limit: u64, trusted: bool) -> Result<Vec<u8>, Box<dyn Error>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        )
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err("input must be a regular file".into());
    }
    if trusted
        && (meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o022 != 0
            || meta.nlink() != 1)
    {
        return Err(
            "trust policy must be administrator-owned and not peer-writable or hardlinked".into(),
        );
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err("input exceeds size limit".into());
    }
    Ok(bytes)
}

pub(crate) fn policy(path: &Path) -> Result<InstallPolicy, Box<dyn Error>> {
    let policy: InstallPolicy = serde_json::from_slice(&read(path, 64 * 1024, true)?)?;
    let os = String::from_utf8(
        read(Path::new("/etc/os-release"), 16 * 1024, false)
            // Many distributions provide /etc/os-release as a link to /usr/lib/os-release.
            .or_else(|_| read(Path::new("/usr/lib/os-release"), 16 * 1024, false))?,
    )?;
    let field = |name: &str| -> Option<&str> {
        os.lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|v| v.trim_matches('"'))
    };
    let version: Vec<u32> = field("VERSION_ID=")
        .ok_or("OS has no version identity")?
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    if std::env::consts::OS != "linux"
        || policy.architecture != std::env::consts::ARCH
        || Some(policy.os_id.as_str()) != field("ID=")
        || policy.os_version != version
    {
        return Err("policy host identity does not match the running host".into());
    }
    Ok(policy)
}

pub fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let usage = "usage: uob-release-manager install STORE POLICY MANIFEST SIGNATURE PAYLOAD | verify STORE POLICY DIGEST";
    let operation = args.first().and_then(|v| v.to_str()).ok_or(usage)?;
    if !((operation == "install" && args.len() == 6) || (operation == "verify" && args.len() == 4))
    {
        return Err(usage.into());
    }
    let policy = policy(Path::new(&args[2]))?;
    let store = ArtifactStore::open(Path::new(&args[1]))?;
    let installed = if operation == "install" {
        let encoded = read(Path::new(&args[3]), 64 * 1024, false)?;
        let signature = read(Path::new(&args[4]), 64, false)?;
        if !fs::symlink_metadata(&args[5])?.is_file() {
            return Err("payload must be a regular file".into());
        }
        store.install(&encoded, &signature, &mut File::open(&args[5])?, &policy)?
    } else {
        store.verify_installed(args[3].to_str().ok_or("invalid digest")?, &policy)?
    };
    println!(
        "verified {} {}",
        installed.manifest.release_id, installed.manifest.compatibility.artifact_digest
    );
    Ok(())
}
