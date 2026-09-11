//! Offline diagnostic policy and startup-only credential resolution.
use serde::Deserialize;
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::PathBuf,
    sync::Arc,
};
use subtle::ConstantTimeEq;
use uob_application::capture::{CaptureGrant, CaptureManager, CapturePermission};
use uob_contracts::{BridgeId, StationId, TargetInstanceId};
use uob_management_adapter::{ManagementCaptureAuthenticator, ManagementCaptureConfiguration};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Configuration {
    allow_capture: bool,
    credentials: Vec<Credential>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    token_file: PathBuf,
    permissions: Vec<Permission>,
    stations: Option<Vec<StationId>>,
    targets: Option<Vec<TargetInstanceId>>,
}
#[derive(Deserialize)]
enum Permission {
    #[serde(rename = "diagnostics:read")]
    Read,
    #[serde(rename = "diagnostics:capture")]
    Capture,
}

pub(crate) struct Validated {
    enabled: bool,
    grants: Vec<(PathBuf, CaptureGrant)>,
}
impl Configuration {
    pub(crate) fn validate(self, bridge: &BridgeId) -> Result<Validated, &'static str> {
        if self.credentials.len() > 32 || (self.allow_capture && self.credentials.is_empty()) {
            return Err("invalid diagnostic credentials");
        }
        let mut grants = Vec::new();
        for credential in self.credentials {
            if !credential.token_file.is_absolute()
                || grants
                    .iter()
                    .any(|(path, _)| *path == credential.token_file)
            {
                return Err("invalid diagnostic credential path");
            }
            let permissions = credential
                .permissions
                .into_iter()
                .map(|p| match p {
                    Permission::Read => CapturePermission::Read,
                    Permission::Capture => CapturePermission::Capture,
                })
                .collect();
            let grant = CaptureGrant::new(
                bridge.clone(),
                permissions,
                credential.stations,
                credential.targets,
            )
            .map_err(|_| "invalid diagnostic grant")?;
            grants.push((credential.token_file, grant));
        }
        Ok(Validated {
            enabled: self.allow_capture,
            grants,
        })
    }
}

struct Credentials(Vec<(Vec<u8>, CaptureGrant)>);
impl ManagementCaptureAuthenticator for Credentials {
    fn authenticate(&self, token: &str) -> Option<CaptureGrant> {
        // Scan every configured entry; no token is logged or propagated into the application.
        let mut result = None;
        for (expected, grant) in &self.0 {
            if bool::from(expected.as_slice().ct_eq(token.as_bytes())) {
                result = Some(grant.clone());
            }
        }
        result
    }
}
impl Validated {
    pub(crate) fn resolve(&self) -> io::Result<ManagementCaptureConfiguration> {
        self.resolve_with_resources(
            uob_application::RuntimeResourceBudget::new(
                uob_application::RuntimeResourceLimits::default(),
            )
            .expect("default limits"),
        )
    }

    pub(crate) fn resolve_with_resources(
        &self,
        resources: uob_application::RuntimeResourceBudget,
    ) -> io::Result<ManagementCaptureConfiguration> {
        let fail = || {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "diagnostic credential unavailable or unsafe",
            )
        };
        let mut entries: Vec<(Vec<u8>, CaptureGrant)> = Vec::new();
        for (path, grant) in &self.grants {
            if fs::canonicalize(path).map_err(|_| fail())? != *path {
                return Err(fail());
            }
            let file = fs::File::open(path).map_err(|_| fail())?;
            let metadata = file.metadata().map_err(|_| fail())?;
            if !metadata.is_file()
                || metadata.nlink() != 1
                || metadata.mode() & 0o007 != 0
                || metadata.len() > 256
            {
                return Err(fail());
            }
            let mut bytes = Vec::new();
            file.take(257).read_to_end(&mut bytes).map_err(|_| fail())?;
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
            if !(32..=128).contains(&bytes.len())
                || !bytes.iter().all(u8::is_ascii_graphic)
                || entries
                    .iter()
                    .any(|(value, _)| bool::from(value.as_slice().ct_eq(&bytes)))
            {
                return Err(fail());
            }
            entries.push((bytes, grant.clone()));
        }
        Ok(ManagementCaptureConfiguration {
            manager: CaptureManager::with_resources(self.enabled, resources),
            authenticator: Arc::new(Credentials(entries)),
        })
    }
}

struct Clock;
impl uob_application::CommandClock for Clock {
    fn now(&self) -> uob_contracts::UtcTimestamp {
        uob_contracts::UtcTimestamp::new(time::OffsetDateTime::now_utc())
    }
}

pub(crate) fn instrument(
    application: uob_application::Application,
    manager: CaptureManager,
) -> uob_application::Application {
    let flow = uob_application::FlowDiagnostics::retained(
        application.identity().runtime.process_instance_id.clone(),
        application.identity().bridge_id.clone(),
        manager,
        Arc::new(Clock),
    );
    application.with_diagnostics(flow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn config(path: &std::path::Path) -> Configuration {
        toml::from_str(&format!(
            "allow_capture=true\n[[credentials]]\ntoken_file='{}'\npermissions=['diagnostics:read','diagnostics:capture']\nstations=['a']\ntargets=['mqtt']\n", path.display()
        )).unwrap()
    }
    #[test]
    fn configuration_is_opt_in_strict_and_offline() {
        let bridge = BridgeId::new("bridge").unwrap();
        assert!(!Configuration::default().validate(&bridge).unwrap().enabled);
        assert!(
            toml::from_str::<Configuration>("allow_capture=true")
                .unwrap()
                .validate(&bridge)
                .is_err()
        );
        assert!(toml::from_str::<Configuration>("allow_captur=true").is_err());
        assert!(
            config(std::path::Path::new("/missing/private/token"))
                .validate(&bridge)
                .is_ok()
        );
        assert!(
            config(std::path::Path::new("relative/token"))
                .validate(&bridge)
                .is_err()
        );
    }
    #[test]
    fn startup_resolves_private_credentials_and_rejects_duplicates_and_unsafe_files() {
        let root = std::env::temp_dir().join(format!("uob-capture-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("token");
        let bridge = BridgeId::new("bridge").unwrap();
        let token = "test-only-capture-secret-with-32-characters";
        let validated = config(&path).validate(&bridge).unwrap();
        assert!(validated.resolve().is_err());
        fs::write(&path, format!("{token}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let runtime = validated.resolve().unwrap();
        assert!(runtime.authenticator.authenticate(token).is_some());
        assert!(runtime.authenticator.authenticate("wrong-token").is_none());
        let grant = runtime.authenticator.authenticate(token).unwrap();
        let wrong_station = uob_application::capture::CaptureFilter {
            bridge: bridge.clone(),
            station: Some(StationId::new("b").unwrap()),
            target: Some(TargetInstanceId::new("mqtt").unwrap()),
        };
        assert!(
            runtime
                .manager
                .start(
                    &grant,
                    wrong_station,
                    uob_application::capture::CaptureLevel::Metadata,
                    None
                )
                .is_err()
        );
        let other = root.join("other-token");
        fs::copy(&path, &other).unwrap();
        let mut duplicate = config(&path);
        duplicate
            .credentials
            .push(config(&other).credentials.pop().unwrap());
        assert!(duplicate.validate(&bridge).unwrap().resolve().is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(validated.resolve().is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&path, "short").unwrap();
        assert!(validated.resolve().is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn startup_emitter_shares_capture_authority_and_daemon_budget() {
        use uob_application::{
            FlowEvidence, FlowStage,
            capture::{CaptureFilter, CaptureLevel},
        };
        use uob_contracts::{ArtifactDigest, ReleaseId};
        let startup = crate::StartupIdentityConfiguration::production(
            BridgeId::new("bridge").unwrap(),
            ReleaseId::new("release").unwrap(),
            ArtifactDigest::new("sha256:release").unwrap(),
        );
        let app = crate::compose(
            uob_target_adapter::TargetRegistry::<(), ()>::new(),
            startup,
            None,
        )
        .unwrap()
        .application;
        let resources = app.health().resources().clone();
        let manager = CaptureManager::with_resources(true, resources.clone());
        let app = instrument(app, manager.clone());
        let span = app.diagnostics().span(None, None, None);
        span.emit(FlowStage::Application, FlowEvidence::Completed);
        assert_eq!(resources.snapshot().trace_ring_bytes, 0);
        let grant = CaptureGrant::new(
            app.identity().bridge_id.clone(),
            vec![CapturePermission::Read, CapturePermission::Capture],
            None,
            None,
        )
        .unwrap();
        let status = manager
            .start(
                &grant,
                CaptureFilter {
                    bridge: app.identity().bridge_id.clone(),
                    station: None,
                    target: None,
                },
                CaptureLevel::Metadata,
                None,
            )
            .unwrap();
        span.emit(FlowStage::Application, FlowEvidence::Completed);
        let read = manager
            .lease(&grant, status.id, false)
            .unwrap()
            .read_after(None)
            .unwrap();
        let record = read.record.unwrap();
        let decoded: uob_contracts::TraceRecord =
            serde_json::from_slice(record.diagnostic.encoded_json()).unwrap();
        assert_eq!(
            decoded.process_instance_id,
            app.runtime_identity().process_instance_id
        );
        assert_eq!(
            resources.snapshot().trace_ring_bytes,
            record.diagnostic.encoded_json().len()
        );
        drop(record);
        manager.stop(&grant, status.id).unwrap();
        assert_eq!(resources.snapshot().trace_ring_bytes, 0);
    }
}
