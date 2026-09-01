use std::{error::Error, fmt};

use uob_application::CredentialReference;
use uob_contracts::Environment;

/// Offline transport facts for one external database destination.
#[derive(Clone, Eq, PartialEq)]
pub struct DatabaseTransportSecurity {
    /// Whether TLS encrypts the provider connection.
    pub tls: bool,
    /// Whether certificate and hostname verification are enabled.
    pub certificate_verification: bool,
    /// Protected runtime credential-file reference, never its contents.
    pub credentials_file: Option<CredentialReference>,
    /// Explicitly isolated test/demo profile required for relaxed transport policy.
    pub explicitly_isolated: bool,
}

/// Applies database transport policy without resolving credentials or opening a connection.
///
/// # Errors
///
/// Production requires verified TLS and a protected credential reference. Staging applies the
/// same policy. Plaintext or missing credentials are accepted only by an explicitly isolated demo.
pub fn validate_database_transport_security(
    environment: Environment,
    security: &DatabaseTransportSecurity,
) -> Result<(), DatabaseSecurityError> {
    let isolated_demo = environment == Environment::Demo && security.explicitly_isolated;
    if !security.tls && !isolated_demo {
        return Err(DatabaseSecurityError::TlsRequired);
    }
    if !security.certificate_verification && !isolated_demo {
        return Err(DatabaseSecurityError::VerificationRequired);
    }
    if security.credentials_file.is_none() && !isolated_demo {
        return Err(DatabaseSecurityError::CredentialsRequired);
    }
    Ok(())
}

/// Sanitized external database transport-policy failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatabaseSecurityError {
    /// The configured environment requires an encrypted connection.
    TlsRequired,
    /// Certificate and hostname verification must remain enabled.
    VerificationRequired,
    /// Credentials must be referenced through the protected credential boundary.
    CredentialsRequired,
}

impl fmt::Display for DatabaseSecurityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "database transport policy {self:?}")
    }
}

impl Error for DatabaseSecurityError {}
