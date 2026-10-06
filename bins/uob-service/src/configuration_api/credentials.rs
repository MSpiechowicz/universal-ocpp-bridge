//! Configuration API credentials: offline grant validation and start-up-only secret reads.
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use serde::Deserialize;
use subtle::ConstantTimeEq;
use uob_application::{AccessGrant, AccessPermission, AccessResourceScope};
use uob_contracts::{AuthenticatedCommandOrigin, BridgeId, Environment, PrincipalId};
use uob_management_adapter::{ManagementConfigurationAuthenticator, token_matches_environment};

const MAX_CREDENTIALS: usize = 32;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Configuration {
    staged_targets_file: Option<PathBuf>,
    credentials: Vec<Credential>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    principal: String,
    token_file: PathBuf,
    permissions: Vec<Permission>,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
enum Permission {
    #[serde(rename = "configuration:read")]
    Read,
    #[serde(rename = "configuration:write")]
    Write,
    #[serde(rename = "configuration:discard")]
    Discard,
}

impl Permission {
    const fn access(self) -> AccessPermission {
        match self {
            Self::Read => AccessPermission::ConfigurationRead,
            Self::Write => AccessPermission::ConfigurationWrite,
            Self::Discard => AccessPermission::DestructiveDisposition,
        }
    }
}

/// Offline-validated credential grants; secrets are read only by [`Validated::resolve`].
pub(crate) struct Validated {
    environment: Environment,
    grants: Vec<(PathBuf, AccessGrant)>,
    configuration_path: Option<PathBuf>,
}

impl Configuration {
    /// Service-writable next-start target section, when the API may apply configuration.
    pub(crate) fn staged_targets_file(&self) -> Option<&Path> {
        self.staged_targets_file.as_deref()
    }

    pub(crate) fn validate(
        &self,
        bridge_id: &BridgeId,
        environment: Environment,
    ) -> Result<Validated, &'static str> {
        const INVALID: &str = "invalid configuration API credentials";
        if self.credentials.len() > MAX_CREDENTIALS {
            return Err(INVALID);
        }
        if let Some(path) = &self.staged_targets_file
            && (!fixed_path(path)
                || path.file_name().is_none()
                || (environment == Environment::Staging
                    && !path.starts_with("/var/lib/uob-staging")))
        {
            return Err(INVALID);
        }
        let mut grants: Vec<(PathBuf, AccessGrant)> = Vec::new();
        for credential in &self.credentials {
            if !fixed_path(&credential.token_file)
                || (environment == Environment::Staging
                    && !credential.token_file.starts_with("/etc/uob-staging"))
                || grants
                    .iter()
                    .any(|(path, _)| *path == credential.token_file)
            {
                return Err(INVALID);
            }
            let principal_id =
                PrincipalId::new(credential.principal.clone()).map_err(|_| INVALID)?;
            let origin = AuthenticatedCommandOrigin::Management { principal_id };
            if grants.iter().any(|(_, grant)| *grant.origin() == origin) {
                return Err(INVALID);
            }
            let mut permissions = Vec::new();
            for permission in &credential.permissions {
                if permissions.contains(&permission.access()) {
                    return Err(INVALID);
                }
                permissions.push(permission.access());
            }
            let grant = AccessGrant::new(
                origin,
                permissions,
                vec![AccessResourceScope::Bridge(bridge_id.clone())],
            )
            .map_err(|_| INVALID)?;
            grants.push((credential.token_file.clone(), grant));
        }
        Ok(Validated {
            environment,
            grants,
            configuration_path: None,
        })
    }
}

impl Validated {
    /// Binds the startup file whose target section the API reads and stages.
    #[must_use]
    pub(crate) fn bound_to(mut self, path: &Path) -> Self {
        self.configuration_path = Some(path.to_path_buf());
        self
    }

    pub(crate) fn configuration_path(&self) -> Option<&Path> {
        self.configuration_path.as_deref()
    }

    /// Reads protected credential files once at start-up. `None` leaves the routes unmounted.
    pub(crate) fn resolve(
        &self,
    ) -> io::Result<Option<Arc<dyn ManagementConfigurationAuthenticator>>> {
        if self.grants.is_empty() {
            return Ok(None);
        }
        let mut entries: Vec<(Vec<u8>, AccessGrant)> = Vec::with_capacity(self.grants.len());
        for (path, grant) in &self.grants {
            let token = read_token(path, self.environment)?;
            if entries
                .iter()
                .any(|(other, _)| bool::from(other.as_slice().ct_eq(&token)))
            {
                return Err(credential_failure());
            }
            entries.push((token, grant.clone()));
        }
        Ok(Some(Arc::new(Credentials(entries))))
    }
}

struct Credentials(Vec<(Vec<u8>, AccessGrant)>);

impl ManagementConfigurationAuthenticator for Credentials {
    fn authenticate(&self, token: &str) -> Option<AccessGrant> {
        // Compare every entry so timing does not reveal which credential matched.
        let mut result = None;
        for (expected, grant) in &self.0 {
            if bool::from(expected.as_slice().ct_eq(token.as_bytes())) {
                result = Some(grant.clone());
            }
        }
        result
    }
}

fn read_token(path: &Path, environment: Environment) -> io::Result<Vec<u8>> {
    if fs::canonicalize(path).map_err(|_| credential_failure())? != path {
        return Err(credential_failure());
    }
    let file = fs::File::open(path).map_err(|_| credential_failure())?;
    let metadata = file.metadata().map_err(|_| credential_failure())?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o007 != 0
        || metadata.len() > 256
    {
        return Err(credential_failure());
    }
    let mut token = Vec::new();
    file.take(257)
        .read_to_end(&mut token)
        .map_err(|_| credential_failure())?;
    if token.last() == Some(&b'\n') {
        token.pop();
    }
    if !std::str::from_utf8(&token).is_ok_and(|value| token_matches_environment(value, environment))
        || !token.iter().all(u8::is_ascii_graphic)
    {
        return Err(credential_failure());
    }
    Ok(token)
}

fn credential_failure() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "configuration API credential unavailable or unsafe",
    )
}

fn fixed_path(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}
