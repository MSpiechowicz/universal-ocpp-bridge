use std::{error::Error, fmt, net::IpAddr};

use uob_application::CredentialReference;
use uob_contracts::Environment;

/// Parsed network authority used only for offline policy validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkEndpoint {
    host: String,
}

impl NetworkEndpoint {
    /// Parses a URL-like endpoint without DNS resolution or connection attempts.
    ///
    /// # Errors
    ///
    /// Rejects an empty authority and embedded user information.
    pub fn parse(value: &str) -> Result<Self, EndpointError> {
        let authority = value
            .split_once("://")
            .map_or(value, |(_, authority)| authority)
            .split('/')
            .next()
            .unwrap_or_default();
        if authority.is_empty() {
            return Err(EndpointError::MissingHost);
        }
        if authority.contains('@') {
            return Err(EndpointError::EmbeddedCredentials);
        }
        let host = if let Some(authority) = authority.strip_prefix('[') {
            authority
                .split_once(']')
                .map(|(host, _)| host)
                .ok_or(EndpointError::InvalidHost)?
        } else {
            authority.split(':').next().unwrap_or_default()
        };
        if host.is_empty() {
            return Err(EndpointError::MissingHost);
        }
        Ok(Self {
            host: host.to_ascii_lowercase(),
        })
    }

    /// Returns whether the literal endpoint is local without resolving a hostname.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        self.host == "localhost"
            || self
                .host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    }
}

/// Transport confidentiality configured by the target factory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportEncryption {
    /// TLS is enabled.
    Tls,
    /// An unencrypted transport is explicitly requested.
    Plaintext,
}

/// Offline facts used by the shared transport policy.
#[derive(Clone, Eq, PartialEq)]
pub struct TransportSecurity {
    /// Parsed destination or listener authority.
    pub endpoint: NetworkEndpoint,
    /// Configured transport encryption.
    pub encryption: TransportEncryption,
    /// Whether TLS certificate and hostname verification are enabled.
    pub certificate_verification: bool,
    /// Runtime credential reference, never credential contents.
    pub credentials: Option<CredentialReference>,
    /// Explicitly isolated demo/test profile required for plaintext.
    pub explicitly_isolated: bool,
}

/// Direction-specific policy applied by a concrete target factory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportPolicy {
    /// Client connection to a broker or remote service.
    Outbound,
    /// Listener such as the direct EMS/SCADA HTTP integration surface.
    Listener,
}

/// Applies common network security rules using only parsed configuration facts.
///
/// # Errors
///
/// Rejects plaintext outside an isolated demo, unverified production TLS, missing
/// production credentials, and unauthenticated/non-TLS non-loopback listeners.
pub fn validate_transport_security(
    environment: Environment,
    policy: TransportPolicy,
    security: &TransportSecurity,
) -> Result<(), TransportPolicyError> {
    let uses_tls = security.encryption == TransportEncryption::Tls;
    if !uses_tls && (environment != Environment::Demo || !security.explicitly_isolated) {
        return Err(TransportPolicyError::PlaintextRequiresIsolatedDemo);
    }
    if environment == Environment::Production && !security.certificate_verification {
        return Err(TransportPolicyError::ProductionVerificationRequired);
    }
    if environment == Environment::Production && security.credentials.is_none() {
        return Err(TransportPolicyError::CredentialsRequired);
    }
    if policy == TransportPolicy::Listener
        && !security.endpoint.is_loopback()
        && (!uses_tls || security.credentials.is_none())
    {
        return Err(TransportPolicyError::NonLoopbackListenerRequiresTlsAndCredentials);
    }
    Ok(())
}

/// Safe endpoint parsing failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointError {
    /// No host was supplied.
    MissingHost,
    /// Bracketed host syntax was invalid.
    InvalidHost,
    /// Secret-like user information was embedded instead of referenced separately.
    EmbeddedCredentials,
}

/// Shared transport-policy failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportPolicyError {
    /// Plaintext is allowed only for explicitly isolated demos/tests.
    PlaintextRequiresIsolatedDemo,
    /// Production TLS must verify certificate and hostname.
    ProductionVerificationRequired,
    /// Production or a public listener requires referenced credentials.
    CredentialsRequired,
    /// A public integration listener requires TLS and credentials.
    NonLoopbackListenerRequiresTlsAndCredentials,
}

impl fmt::Display for TransportPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "target transport policy {self:?}")
    }
}

impl Error for TransportPolicyError {}

#[cfg(test)]
mod tests {
    use uob_application::CredentialReference;
    use uob_contracts::Environment;

    use super::{
        EndpointError, NetworkEndpoint, TransportEncryption, TransportPolicy, TransportPolicyError,
        TransportSecurity, validate_transport_security,
    };

    fn security(
        endpoint: &str,
        encryption: TransportEncryption,
        verified: bool,
        credentials: bool,
        isolated: bool,
    ) -> TransportSecurity {
        TransportSecurity {
            endpoint: NetworkEndpoint::parse(endpoint).expect("fixture endpoint"),
            encryption,
            certificate_verification: verified,
            credentials: credentials.then(|| {
                CredentialReference::new("/run/secrets/target").expect("credential reference")
            }),
            explicitly_isolated: isolated,
        }
    }

    #[test]
    fn endpoint_parsing_is_literal_and_rejects_embedded_credentials() {
        assert!(
            NetworkEndpoint::parse("mqtt://localhost:1883")
                .unwrap()
                .is_loopback()
        );
        assert!(NetworkEndpoint::parse("[::1]:9080").unwrap().is_loopback());
        assert!(!NetworkEndpoint::parse("broker:1883").unwrap().is_loopback());
        assert_eq!(
            NetworkEndpoint::parse("mqtt://user:secret@broker:1883"),
            Err(EndpointError::EmbeddedCredentials)
        );
    }

    #[test]
    fn isolated_demo_allows_plaintext_outbound_peer() {
        let demo = security(
            "broker:1883",
            TransportEncryption::Plaintext,
            false,
            false,
            true,
        );
        assert_eq!(
            validate_transport_security(Environment::Demo, TransportPolicy::Outbound, &demo),
            Ok(())
        );
    }

    #[test]
    fn public_listener_requires_tls_and_credentials() {
        let listener = security(
            "0.0.0.0:9080",
            TransportEncryption::Plaintext,
            false,
            false,
            true,
        );
        assert_eq!(
            validate_transport_security(Environment::Demo, TransportPolicy::Listener, &listener),
            Err(TransportPolicyError::NonLoopbackListenerRequiresTlsAndCredentials)
        );
    }

    #[test]
    fn production_requires_verified_tls_and_referenced_credentials() {
        let unverified = security(
            "broker.example:8883",
            TransportEncryption::Tls,
            false,
            true,
            false,
        );
        assert_eq!(
            validate_transport_security(
                Environment::Production,
                TransportPolicy::Outbound,
                &unverified
            ),
            Err(TransportPolicyError::ProductionVerificationRequired)
        );
    }
}
