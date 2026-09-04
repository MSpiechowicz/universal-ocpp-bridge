use std::{collections::BTreeSet, path::PathBuf};

use serde::Deserialize;
use uob_application::{AccessGrant, AccessPermission, AccessResourceScope, CredentialReference};
use uob_contracts::{
    AuthenticatedCommandOrigin, BridgeId, PrincipalId, StationId, TargetInstanceId,
};

use super::bounded_file::read_bounded_file;

const MAX_CREDENTIAL_FILE_BYTES: u64 = 64 * 1024;

/// One authenticated integration principal and the immutable grant it carries.
///
/// The grant always uses [`AuthenticatedCommandOrigin::Target`]. An integration credential
/// therefore cannot name a management principal, so it can never satisfy the management
/// listener's management-origin requirement.
pub struct IntegrationPrincipal {
    token: String,
    permissions: Vec<AccessPermission>,
    resource_scopes: Vec<AccessResourceScope>,
    grant: AccessGrant,
}

impl IntegrationPrincipal {
    /// Returns the validated application grant bound to this credential.
    #[must_use]
    pub const fn grant(&self) -> &AccessGrant {
        &self.grant
    }

    /// Returns the coarse application permissions this credential was granted.
    #[must_use]
    pub fn permissions(&self) -> &[AccessPermission] {
        &self.permissions
    }

    /// Returns the canonical resource boundary this credential was granted.
    #[must_use]
    pub fn resource_scopes(&self) -> &[AccessResourceScope] {
        &self.resource_scopes
    }
}

/// Every integration principal configured for one target instance.
#[derive(Default)]
pub struct IntegrationCredentials {
    principals: Vec<IntegrationPrincipal>,
}

impl IntegrationCredentials {
    /// Returns the principal for an exact bearer token.
    ///
    /// Every candidate is compared so the work does not depend on which principal matched.
    #[must_use]
    pub fn authenticate(&self, token: &str) -> Option<&IntegrationPrincipal> {
        let mut matched = None;
        for principal in &self.principals {
            if constant_time_eq(principal.token.as_bytes(), token.as_bytes()) && matched.is_none() {
                matched = Some(principal);
            }
        }
        matched
    }

    /// Returns whether no integration principal is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.principals.is_empty()
    }

    /// Returns the number of configured integration principals.
    #[must_use]
    pub fn len(&self) -> usize {
        self.principals.len()
    }
}

/// Compares two secrets without an early return that would leak the matching prefix length.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = u8::from(left.len() != right.len());
    for index in 0..left.len().max(right.len()) {
        let left_byte = left.get(index).copied().unwrap_or_default();
        let right_byte = right.get(index).copied().unwrap_or_default();
        difference |= left_byte ^ right_byte;
    }
    difference == 0
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFile {
    #[serde(default)]
    principals: Vec<PrincipalEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalEntry {
    id: String,
    token: String,
    permissions: Vec<PermissionEntry>,
    #[serde(default)]
    bridges: Vec<String>,
    #[serde(default)]
    stations: Vec<StationEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StationEntry {
    bridge_id: String,
    station_id: String,
}

/// Deliberately narrow permission vocabulary. An integration credential cannot name a
/// diagnostic, capture, or administration permission because this file declares none.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PermissionEntry {
    Read,
    Control,
}

impl PermissionEntry {
    const fn permission(self) -> AccessPermission {
        match self {
            Self::Read => AccessPermission::Read,
            Self::Control => AccessPermission::Control,
        }
    }
}

/// Reads and validates the scoped integration credential file.
///
/// # Errors
///
/// Returns a stable sanitized reason code when the file is unavailable, world-readable, invalid,
/// or declares a principal without a permission or canonical resource scope.
pub(crate) fn resolve_credentials(
    reference: Option<&CredentialReference>,
    target_instance_id: &TargetInstanceId,
) -> Result<IntegrationCredentials, &'static str> {
    let Some(reference) = reference else {
        return Ok(IntegrationCredentials::default());
    };
    let source = read_bounded_file(
        &PathBuf::from(reference.as_str()),
        MAX_CREDENTIAL_FILE_BYTES,
        true,
    )
    .map_err(|()| "ems_scada_http.credentials_unavailable")?;
    let document: CredentialFile =
        toml::from_slice(&source).map_err(|_| "ems_scada_http.credentials_invalid")?;
    if document.principals.is_empty() {
        return Err("ems_scada_http.credentials_invalid");
    }

    let mut identities = BTreeSet::new();
    let mut tokens = BTreeSet::new();
    let mut principals = Vec::with_capacity(document.principals.len());
    for entry in document.principals {
        principals.push(principal(
            entry,
            target_instance_id,
            &mut identities,
            &mut tokens,
        )?);
    }
    Ok(IntegrationCredentials { principals })
}

fn principal(
    entry: PrincipalEntry,
    target_instance_id: &TargetInstanceId,
    identities: &mut BTreeSet<String>,
    tokens: &mut BTreeSet<String>,
) -> Result<IntegrationPrincipal, &'static str> {
    if entry.token.is_empty() || !entry.token.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("ems_scada_http.credentials_invalid");
    }
    if !identities.insert(entry.id.clone()) || !tokens.insert(entry.token.clone()) {
        return Err("ems_scada_http.credentials_invalid");
    }
    let principal_id =
        PrincipalId::new(entry.id).map_err(|_| "ems_scada_http.credentials_invalid")?;
    let mut resource_scopes = Vec::new();
    for bridge in entry.bridges {
        resource_scopes.push(AccessResourceScope::Bridge(
            BridgeId::new(bridge).map_err(|_| "ems_scada_http.credentials_invalid")?,
        ));
    }
    for station in entry.stations {
        resource_scopes.push(AccessResourceScope::Station {
            bridge_id: BridgeId::new(station.bridge_id)
                .map_err(|_| "ems_scada_http.credentials_invalid")?,
            station_id: StationId::new(station.station_id)
                .map_err(|_| "ems_scada_http.credentials_invalid")?,
        });
    }
    let permissions: Vec<AccessPermission> = entry
        .permissions
        .into_iter()
        .map(PermissionEntry::permission)
        .collect();
    let grant = AccessGrant::new(
        AuthenticatedCommandOrigin::Target {
            target_instance_id: target_instance_id.clone(),
            principal_id,
        },
        permissions.clone(),
        resource_scopes.clone(),
    )
    .map_err(|_| "ems_scada_http.credentials_invalid")?;
    Ok(IntegrationPrincipal {
        token: entry.token,
        permissions,
        resource_scopes,
        grant,
    })
}
