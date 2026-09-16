//! Production-only release supervisor read capability.
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use serde::Deserialize;
use subtle::ConstantTimeEq;
use uob_contracts::Environment;
use uob_management_adapter::{
    ManagementReleaseReadAuthenticator, ManagementReleaseReadConfiguration,
};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Configuration {
    supervisor_socket: Option<PathBuf>,
    token_file: Option<PathBuf>,
}

pub(crate) struct Validated {
    supervisor_socket: Option<PathBuf>,
    token_file: Option<PathBuf>,
    environment: Environment,
}

impl Configuration {
    pub(crate) fn validate(self, environment: Environment) -> Result<Validated, &'static str> {
        let configured = self.supervisor_socket.is_some() || self.token_file.is_some();
        if !configured {
            return Ok(Validated {
                supervisor_socket: None,
                token_file: None,
                environment,
            });
        }
        let (Some(supervisor_socket), Some(token_file)) = (self.supervisor_socket, self.token_file)
        else {
            return Err("invalid release read configuration");
        };
        if environment != Environment::Production
            || !fixed_path(&supervisor_socket)
            || !fixed_path(&token_file)
        {
            return Err("invalid release read configuration");
        }
        Ok(Validated {
            supervisor_socket: Some(supervisor_socket),
            token_file: Some(token_file),
            environment,
        })
    }
}

impl Validated {
    pub(crate) fn resolve(&self) -> io::Result<Option<ManagementReleaseReadConfiguration>> {
        let (Some(supervisor_socket), Some(token_file)) =
            (&self.supervisor_socket, &self.token_file)
        else {
            return Ok(None);
        };
        let fail = || {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "release read credential unavailable or unsafe",
            )
        };
        if fs::canonicalize(token_file).map_err(|_| fail())? != *token_file {
            return Err(fail());
        }
        let file = fs::File::open(token_file).map_err(|_| fail())?;
        let metadata = file.metadata().map_err(|_| fail())?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o007 != 0
            || metadata.len() > 256
        {
            return Err(fail());
        }
        let mut token = Vec::new();
        file.take(257).read_to_end(&mut token).map_err(|_| fail())?;
        if token.last() == Some(&b'\n') {
            token.pop();
        }
        if !std::str::from_utf8(&token).is_ok_and(|token| {
            uob_management_adapter::token_matches_environment(token, self.environment)
        }) || !token.iter().all(u8::is_ascii_graphic)
        {
            return Err(fail());
        }
        Ok(Some(ManagementReleaseReadConfiguration {
            supervisor_socket: supervisor_socket.clone(),
            authenticator: Arc::new(Credentials(token)),
        }))
    }
}

struct Credentials(Vec<u8>);
impl ManagementReleaseReadAuthenticator for Credentials {
    fn authenticate(&self, token: &str) -> bool {
        bool::from(self.0.as_slice().ct_eq(token.as_bytes()))
    }
}

fn fixed_path(path: &Path) -> bool {
    path.is_absolute()
        && !path.as_os_str().is_empty()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn capability_is_disabled_by_default_and_production_only_when_configured() {
        assert!(
            Configuration::default()
                .validate(Environment::Production)
                .unwrap()
                .resolve()
                .unwrap()
                .is_none()
        );
        for environment in [Environment::Staging, Environment::Demo] {
            assert!(toml::from_str::<Configuration>(
                "supervisor_socket='/run/uob-release-manager/control.sock'\ntoken_file='/etc/uob/release-read'"
            )
            .unwrap()
            .validate(environment)
            .is_err());
        }
        assert!(
            toml::from_str::<Configuration>("supervisor_socket='/run/control.sock'")
                .unwrap()
                .validate(Environment::Production)
                .is_err()
        );
        assert!(
            toml::from_str::<Configuration>(
                "supervisor_socket='../control.sock'\ntoken_file='/etc/uob/release-read'"
            )
            .unwrap()
            .validate(Environment::Production)
            .is_err()
        );
    }

    #[test]
    fn resolver_requires_a_private_environment_bound_token() {
        let root = std::env::temp_dir().join(format!("uob-release-read-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let token = root.join("token");
        let configuration: Configuration = toml::from_str(&format!(
            "supervisor_socket='/run/uob-release-manager/control.sock'\ntoken_file='{}'",
            token.display()
        ))
        .unwrap();
        let validated = configuration.validate(Environment::Production).unwrap();
        assert!(validated.resolve().is_err());
        fs::write(
            &token,
            "uob1.staging.wrong-environment-token-that-is-long-enough",
        )
        .unwrap();
        fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(validated.resolve().is_err());
        let credential = "uob1.production.release-read-token-that-is-long-enough";
        fs::write(&token, credential).unwrap();
        let resolved = validated.resolve().unwrap().expect("configured capability");
        assert!(resolved.authenticator.authenticate(credential));
        assert!(
            !resolved
                .authenticator
                .authenticate("uob1.production.wrong-token-that-is-long-enough")
        );
        fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(validated.resolve().is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
