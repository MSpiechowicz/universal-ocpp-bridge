use std::{collections::BTreeSet, error::Error, fmt, net::SocketAddr, sync::Arc};

use uob_application::{
    AccessGrant, AccessPolicy, CommandAdmissionPort, CredentialReference,
    ScopedCommandAdmissionPort,
};
use uob_contracts::AuthenticatedCommandOrigin;

/// Default management listener; external interfaces always require deliberate configuration.
pub const DEFAULT_MANAGEMENT_LISTEN_ADDRESS: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8080);

/// TLS identity references for a deliberately exposed remote management listener.
#[derive(Clone, Eq, PartialEq)]
pub struct ManagementTlsConfiguration {
    /// PEM certificate chain resolved only by the management adapter at startup.
    pub certificate_chain: CredentialReference,
    /// PEM private key resolved only by the management adapter at startup.
    pub private_key: CredentialReference,
}

/// One credential reference and its application access grant.
#[derive(Clone, Eq, PartialEq)]
pub struct ManagementCredentialConfiguration {
    /// Secret reference resolved outside the safe configuration model.
    pub credential: CredentialReference,
    /// Validated principal, permission, and canonical resource boundary.
    pub grant: AccessGrant,
}

impl ManagementCredentialConfiguration {
    /// Applies this authenticated HTTP credential's immutable grant to the common application
    /// command path.
    #[must_use]
    pub fn scoped_commands<P: Send + 'static>(
        &self,
        inner: Arc<dyn CommandAdmissionPort<P>>,
    ) -> ScopedCommandAdmissionPort<P> {
        ScopedCommandAdmissionPort::new(inner, AccessPolicy::single(self.grant.clone()))
    }
}

/// Offline management listener policy used before binding or resolving secrets.
///
/// This type intentionally omits `Debug`: even credential locations stay out of routine
/// configuration diagnostics.
#[derive(Clone, Eq, PartialEq)]
pub struct ManagementListenerConfiguration {
    /// Exact socket address requested by trusted startup configuration.
    pub listen_address: SocketAddr,
    /// Deliberate acknowledgement that the listener may accept remote connections.
    pub remote_access_enabled: bool,
    /// TLS identity required whenever the address is not loopback.
    pub tls: Option<ManagementTlsConfiguration>,
    /// Referenced credentials with explicit application grants.
    pub credentials: Vec<ManagementCredentialConfiguration>,
}

impl Default for ManagementListenerConfiguration {
    fn default() -> Self {
        Self {
            listen_address: DEFAULT_MANAGEMENT_LISTEN_ADDRESS,
            remote_access_enabled: false,
            tls: None,
            credentials: vec![],
        }
    }
}

impl ManagementListenerConfiguration {
    /// Validates exposure and credential identity without binding, resolving secrets, or loading
    /// certificates.
    ///
    /// # Errors
    ///
    /// Rejects implicit external binding, non-TLS remote access, missing scoped credentials,
    /// non-management grants, and duplicate principal or credential references.
    pub fn validate(&self) -> Result<(), ManagementListenerPolicyError> {
        let is_remote = !self.listen_address.ip().is_loopback();
        if is_remote && !self.remote_access_enabled {
            return Err(ManagementListenerPolicyError::RemoteAccessNotEnabled);
        }
        if is_remote && self.tls.is_none() {
            return Err(ManagementListenerPolicyError::RemoteTlsRequired);
        }
        if is_remote && self.credentials.is_empty() {
            return Err(ManagementListenerPolicyError::RemoteCredentialsRequired);
        }

        let mut principals = BTreeSet::new();
        let mut credential_references = BTreeSet::new();
        for credential in &self.credentials {
            let AuthenticatedCommandOrigin::Management { principal_id } = credential.grant.origin()
            else {
                return Err(ManagementListenerPolicyError::ManagementOriginRequired);
            };
            if !principals.insert(principal_id.as_str()) {
                return Err(ManagementListenerPolicyError::DuplicatePrincipal);
            }
            if !credential_references.insert(credential.credential.as_str()) {
                return Err(ManagementListenerPolicyError::DuplicateCredentialReference);
            }
        }
        Ok(())
    }
}

/// Safe offline management-listener policy failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementListenerPolicyError {
    /// A non-loopback address was supplied without explicit remote enablement.
    RemoteAccessNotEnabled,
    /// A remote listener omitted its TLS server identity.
    RemoteTlsRequired,
    /// A remote listener has no credential with an explicit permission and resource grant.
    RemoteCredentialsRequired,
    /// A management credential carried a target or internal bridge origin.
    ManagementOriginRequired,
    /// Two credential entries resolve to the same application principal.
    DuplicatePrincipal,
    /// Two principals reference the same transport credential.
    DuplicateCredentialReference,
}

impl fmt::Display for ManagementListenerPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "management listener policy {self:?}")
    }
}

impl Error for ManagementListenerPolicyError {}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use uob_application::{
        AccessGrant, AccessPermission, AccessPolicyError, AccessResourceScope, CredentialReference,
    };
    use uob_contracts::{AuthenticatedCommandOrigin, BridgeId, PrincipalId, StationId};

    use super::{
        DEFAULT_MANAGEMENT_LISTEN_ADDRESS, ManagementCredentialConfiguration,
        ManagementListenerConfiguration, ManagementListenerPolicyError, ManagementTlsConfiguration,
    };

    fn reference(value: &str) -> CredentialReference {
        CredentialReference::new(value).unwrap()
    }

    fn credential(permissions: Vec<AccessPermission>) -> ManagementCredentialConfiguration {
        ManagementCredentialConfiguration {
            credential: reference("/run/uob/management/operator.token"),
            grant: AccessGrant::new(
                AuthenticatedCommandOrigin::Management {
                    principal_id: PrincipalId::new("operator-a").unwrap(),
                },
                permissions,
                vec![AccessResourceScope::Station {
                    bridge_id: BridgeId::new("bridge-a").unwrap(),
                    station_id: StationId::new("station-a").unwrap(),
                }],
            )
            .unwrap(),
        }
    }

    fn remote() -> ManagementListenerConfiguration {
        ManagementListenerConfiguration {
            listen_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8443),
            remote_access_enabled: true,
            tls: Some(ManagementTlsConfiguration {
                certificate_chain: reference("/run/uob/management/server.crt"),
                private_key: reference("/run/uob/management/server.key"),
            }),
            credentials: vec![credential(vec![AccessPermission::Read])],
        }
    }

    #[test]
    fn default_configuration_is_loopback_only() {
        let configuration = ManagementListenerConfiguration::default();
        assert_eq!(
            configuration.listen_address,
            DEFAULT_MANAGEMENT_LISTEN_ADDRESS
        );
        assert!(configuration.listen_address.ip().is_loopback());
        assert!(!configuration.remote_access_enabled);
        assert_eq!(configuration.validate(), Ok(()));
    }

    #[test]
    fn remote_configuration_requires_explicit_enablement_tls_and_credentials() {
        let mut configuration = remote();
        configuration.remote_access_enabled = false;
        assert_eq!(
            configuration.validate(),
            Err(ManagementListenerPolicyError::RemoteAccessNotEnabled)
        );
        configuration.remote_access_enabled = true;
        configuration.tls = None;
        assert_eq!(
            configuration.validate(),
            Err(ManagementListenerPolicyError::RemoteTlsRequired)
        );
        configuration.tls = remote().tls;
        configuration.credentials.clear();
        assert_eq!(
            configuration.validate(),
            Err(ManagementListenerPolicyError::RemoteCredentialsRequired)
        );
    }

    #[test]
    fn control_credentials_cannot_be_created_without_a_resource_scope() {
        assert_eq!(
            AccessGrant::new(
                AuthenticatedCommandOrigin::Management {
                    principal_id: PrincipalId::new("operator-a").unwrap(),
                },
                vec![AccessPermission::Control],
                vec![],
            ),
            Err(AccessPolicyError::MissingResourceScope)
        );
    }

    #[test]
    fn scoped_remote_observer_configuration_is_valid() {
        assert_eq!(remote().validate(), Ok(()));
    }
}
