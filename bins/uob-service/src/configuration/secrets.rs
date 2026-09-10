//! Offline preflight reads references without logging contents or contacting peers.
use super::{ConfigurationLoadError, FileConfiguration};
use std::{fs, io::Read, os::unix::fs::MetadataExt, path::Path};

pub(super) fn check(path: &Path) -> Result<(), ConfigurationLoadError> {
    let fail = || ConfigurationLoadError::UnavailableSecret;
    let document = fs::read_to_string(path).map_err(|_| fail())?;
    let config: FileConfiguration = toml::from_str(&document).map_err(|_| fail())?;
    if config.bridge.environment != uob_contracts::Environment::Production {
        return Err(fail());
    }
    let mut references = Vec::new();
    if let Some(path) = config.events.credentials_file {
        references.push(path);
    }
    for target in config.targets.into_iter().filter(|t| t.enabled) {
        if let Some(path) = target.transport.and_then(|t| t.credentials_file) {
            references.push(path);
        }
        for (key, value) in target.settings {
            if super::is_credential_field(&key) {
                references.push(value.as_str().ok_or_else(fail)?.to_owned());
            }
        }
    }
    if references.len() > 64 {
        return Err(fail());
    }
    for reference in references {
        let path = Path::new(&reference);
        if !path.is_absolute() || fs::canonicalize(path).map_err(|_| fail())? != path {
            return Err(fail());
        }
        let metadata = fs::symlink_metadata(path).map_err(|_| fail())?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o007 != 0
            || metadata.len() == 0
            || metadata.len() > 64 * 1024
        {
            return Err(fail());
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(|_| fail())?
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| fail())?;
        if bytes.is_empty() || bytes.len() > 64 * 1024 {
            return Err(fail());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn production_secret_references_are_required_readable_private_and_nonempty() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("uob-secret-check-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let config = root.join("bridge.toml");
        let secret = root.join("token");
        fs::write(
            &config,
            format!(
                "[bridge]\nid='production'\n[events]\ncredentials_file='{}'\n",
                secret.display()
            ),
        )
        .unwrap();
        assert!(check(&config).is_err());
        fs::write(&secret, "private-test-token").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(check(&config).is_ok());
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(check(&config).is_err());
        fs::write(&secret, "").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(check(&config).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
