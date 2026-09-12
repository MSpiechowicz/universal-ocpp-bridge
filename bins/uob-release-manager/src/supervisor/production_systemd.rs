//! Fixed-unit Linux adapter. Installation and live drain transport are host composition.
use super::promotion::{ProcessFuture, ProductionProcess, Start};
use crate::{artifacts::InstallError, qualification};
use std::{
    fs::{self, File},
    io::Read,
    os::unix::fs::MetadataExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};

/// Uses the packaged `uob-production@.service` and its fixed production directories.
/// The service template must be administrator-owned and must not be enabled at boot:
/// the activation owner recovers the journal before starting the selected instance.
/// Existing `uob.service` belongs to the same slice and is included in every stop.
#[derive(Default)]
pub struct SystemdProduction;

impl ProductionProcess for SystemdProduction {
    fn stop_and_confirm(&mut self) -> ProcessFuture<'_> {
        Box::pin(async {
            systemctl("stop", "uob-production.slice").await?;
            let path = Path::new("/sys/fs/cgroup/uob.slice/uob-production.slice/cgroup.events");
            match fs::read_to_string(path) {
                Ok(events) if events.lines().any(|line| line == "populated 0") => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // An absent slice counts as stopped only on a mounted cgroup v2 host.
                    if Path::new("/sys/fs/cgroup/cgroup.controllers").is_file() {
                        Ok(())
                    } else {
                        Err(rejected())
                    }
                }
                _ => Err(rejected()),
            }
        })
    }
    fn start_and_confirm(&mut self, start: Start) -> ProcessFuture<'_> {
        Box::pin(async move {
            validate(&start)?;
            // No --no-block: Type=notify start completion requires the service's READY=1.
            systemctl("start", &format!("uob-production@{}.service", start.digest)).await
        })
    }
}

fn validate(start: &Start) -> Result<(), InstallError> {
    if !crate::artifacts::manifest::digest_name(&start.digest)
        || start.binary
            != Path::new("/var/lib/uob-releases/artifacts")
                .join(&start.digest)
                .join("bin/uob")
        || start.configuration != Path::new("/etc/uob/bridge.toml")
        || start.operational_database != Path::new("/var/lib/uob/operational.sqlite3")
    {
        return Err(rejected());
    }
    let state = fs::symlink_metadata("/var/lib/uob")?;
    if !state.is_dir() || state.uid() != start.uid || state.gid() != start.gid {
        return Err(rejected());
    }
    let mut bytes = Vec::new();
    File::open(&start.configuration)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 || qualification::digest(&bytes) != start.configuration_digest {
        return Err(rejected());
    }
    Ok(())
}

struct Helper(Child);
impl Drop for Helper {
    fn drop(&mut self) {
        // Killing the helper does NOT cancel PID1's job. The supervisor records uncertain
        // activation, and recovery always issues a complete slice stop before starting.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
async fn systemctl(operation: &str, unit: &str) -> Result<(), InstallError> {
    let mut helper = Helper(
        Command::new("/usr/bin/systemctl")
            .args(["--system", "--no-ask-password", operation, unit])
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    loop {
        if let Some(status) = helper.0.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(rejected())
            };
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
fn rejected() -> InstallError {
    InstallError::Rejected("production systemd operation unconfirmed")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_and_digest_cannot_select_arbitrary_units_or_staging_inputs() {
        let mut start = Start {
            digest: "a".repeat(64),
            binary: "/tmp/untrusted/bin/uob".into(),
            configuration: "/etc/uob/bridge.toml".into(),
            operational_database: "/var/lib/uob/operational.sqlite3".into(),
            configuration_digest: "b".repeat(64),
            uid: 1,
            gid: 1,
        };
        assert!(validate(&start).is_err());
        start.digest = "../../uob-staging".into();
        assert!(validate(&start).is_err());
    }
}
