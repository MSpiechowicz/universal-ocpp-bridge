use super::*;
use uob_contracts::{ArtifactDigest, BridgeId, ProcessInstanceId, ReleaseId, RuntimeIdentity};

struct Layout(PathBuf);
impl Layout {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("uob-deployment-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self(fs::canonicalize(root).unwrap())
    }
    fn environment(&self, name: &str) -> DeploymentLayout {
        let state = self.0.join(format!("{name}-state"));
        let runtime = self.0.join(format!("{name}-run"));
        for path in [&state, &runtime] {
            fs::create_dir(path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        DeploymentLayout::new(state, runtime).unwrap()
    }
}
impl Drop for Layout {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn identity(environment: Environment) -> ServiceIdentity {
    ServiceIdentity {
        bridge_id: BridgeId::new("test-bridge").unwrap(),
        runtime: RuntimeIdentity {
            environment,
            release_id: ReleaseId::new("test-release").unwrap(),
            release_digest: ArtifactDigest::new("sha256:test").unwrap(),
            process_instance_id: ProcessInstanceId::new(uuid::Uuid::new_v4().to_string()).unwrap(),
        },
        selected_target_id: None,
    }
}

#[tokio::test]
async fn distinct_databases_survive_restart_and_reject_cross_environment_reuse() {
    let fixture = Layout::new();
    let prod = fixture.environment("production");
    let staging = fixture.environment("staging");
    let prod_identity = identity(Environment::Production);
    let first = prod.open(&prod_identity).unwrap();
    // Production has no dependency on staging's files or process.
    assert!(prod.state.join("operational.sqlite3").is_file());
    assert!(prod.open(&prod_identity).is_err(), "duplicate owner");
    let second = staging.open(&identity(Environment::Staging)).unwrap();
    assert!(staging.state.join("operational.sqlite3").is_file());
    assert_eq!(first.store.runtime_configuration().journal_mode, "wal");
    assert_eq!(second.store.runtime_configuration().journal_mode, "wal");
    first.shutdown(Duration::from_secs(2)).await.unwrap();
    drop(first);
    assert!(prod.open(&identity(Environment::Staging)).is_err());
    let mut renamed = prod_identity.clone();
    renamed.bridge_id = BridgeId::new("another-bridge").unwrap();
    assert!(prod.open(&renamed).is_err());
    let restarted = prod.open(&prod_identity).unwrap();
    restarted.shutdown(Duration::from_secs(2)).await.unwrap();
    second.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[test]
fn ambiguous_layout_and_unbound_existing_database_are_rejected() {
    assert!(DeploymentLayout::new("relative".into(), "/run/test".into()).is_err());
    assert!(DeploymentLayout::new("/var/lib/test".into(), "/var/lib/test/run".into()).is_err());
    assert!(DeploymentLayout::new("/var/lib/test:other".into(), "/run/test".into()).is_err());
    let fixture = Layout::new();
    let layout = fixture.environment("unbound");
    fs::write(layout.state.join("operational.sqlite3"), b"existing state").unwrap();
    assert!(layout.open(&identity(Environment::Production)).is_err());
    assert!(!layout.state.join("identity.json").exists());
}

#[cfg(unix)]
#[test]
fn symlinks_hardlinks_and_shared_directory_permissions_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let fixture = Layout::new();
    let layout = fixture.environment("links");
    let source = fixture.0.join("existing");
    fs::write(&source, b"untouched").unwrap();
    let database = layout.state.join("operational.sqlite3");
    symlink(&source, &database).unwrap();
    assert!(layout.open(&identity(Environment::Production)).is_err());
    fs::remove_file(&database).unwrap();
    fs::hard_link(&source, &database).unwrap();
    assert!(layout.open(&identity(Environment::Production)).is_err());
    assert_eq!(fs::read(&source).unwrap(), b"untouched");
    fs::remove_file(&database).unwrap();
    fs::set_permissions(&layout.state, fs::Permissions::from_mode(0o750)).unwrap();
    assert!(layout.open(&identity(Environment::Production)).is_err());
}
