//! Runtime security gates for isolated controls.

use std::{error::Error, fmt};

use uob_contracts::Environment;

use crate::{
    AuthorizationProviderDescriptor, artifact_provider::ArtifactProviderDescriptor,
    certificate_provider::CertificateProviderDescriptor,
};

/// Controls that are valid only in an explicitly isolated environment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IsolatedControl {
    /// Simulator scenario or fault-injection endpoint.
    Simulator,
    /// Local mock-checkout endpoint.
    MockCheckout,
    /// Authorization provider that accepts test credentials.
    TestAuthorizationProvider,
    /// Artifact provider that serves test firmware or accepts test uploads.
    TestArtifactProvider,
    /// Certificate provider backed by a demo-only certificate authority.
    TestCertificateProvider,
    /// Test-only artifact, certificate, signature or upload destination.
    TestCredential,
}

/// Security policy derived only from trusted runtime identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeSecurityPolicy {
    environment: Environment,
}

impl RuntimeSecurityPolicy {
    /// Creates a policy for the composition-root-owned environment.
    #[must_use]
    pub const fn new(environment: Environment) -> Self {
        Self { environment }
    }

    /// Rejects simulator and mock-checkout controls in production.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityPolicyError::IsolatedControlInProduction`] in production.
    pub const fn authorize_isolated_control(
        self,
        control: IsolatedControl,
    ) -> Result<(), SecurityPolicyError> {
        if matches!(self.environment, Environment::Production) {
            return Err(SecurityPolicyError::IsolatedControlInProduction(control));
        }
        Ok(())
    }

    /// Rejects test-only authorization providers from trusted production composition.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityPolicyError::IsolatedControlInProduction`] when a test-only provider is
    /// selected in production.
    pub const fn authorize_authorization_provider(
        self,
        provider: AuthorizationProviderDescriptor,
    ) -> Result<(), SecurityPolicyError> {
        if provider.test_only && matches!(self.environment, Environment::Production) {
            return Err(SecurityPolicyError::IsolatedControlInProduction(
                IsolatedControl::TestAuthorizationProvider,
            ));
        }
        Ok(())
    }

    /// Rejects test-only artifact providers from trusted production composition.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityPolicyError::IsolatedControlInProduction`] when a test-only provider is
    /// selected in production.
    pub const fn authorize_artifact_provider(
        self,
        provider: ArtifactProviderDescriptor,
    ) -> Result<(), SecurityPolicyError> {
        self.reject_test_only(provider.test_only, IsolatedControl::TestArtifactProvider)
    }

    /// Rejects test-only certificate providers from trusted production composition.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityPolicyError::IsolatedControlInProduction`] when a test-only provider is
    /// selected in production.
    pub const fn authorize_certificate_provider(
        self,
        provider: CertificateProviderDescriptor,
    ) -> Result<(), SecurityPolicyError> {
        self.reject_test_only(provider.test_only, IsolatedControl::TestCertificateProvider)
    }

    /// Rejects provider material marked test-only, such as demo firmware, roots or signed
    /// certificates, before production can send it to a station or trust it.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityPolicyError::IsolatedControlInProduction`] for test-only material in
    /// production.
    pub const fn authorize_provider_material(
        self,
        test_only: bool,
    ) -> Result<(), SecurityPolicyError> {
        self.reject_test_only(test_only, IsolatedControl::TestCredential)
    }

    const fn reject_test_only(
        self,
        test_only: bool,
        control: IsolatedControl,
    ) -> Result<(), SecurityPolicyError> {
        if test_only && matches!(self.environment, Environment::Production) {
            return Err(SecurityPolicyError::IsolatedControlInProduction(control));
        }
        Ok(())
    }
}

/// Stable policy rejection safe for diagnostics and API errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityPolicyError {
    /// A test-only endpoint was requested in production.
    IsolatedControlInProduction(IsolatedControl),
}

impl fmt::Display for SecurityPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IsolatedControlInProduction(IsolatedControl::Simulator) => {
                formatter.write_str("simulator controls are unavailable in production")
            }
            Self::IsolatedControlInProduction(IsolatedControl::MockCheckout) => {
                formatter.write_str("mock checkout is unavailable in production")
            }
            Self::IsolatedControlInProduction(IsolatedControl::TestAuthorizationProvider) => {
                formatter.write_str("test authorization providers are unavailable in production")
            }
            Self::IsolatedControlInProduction(IsolatedControl::TestArtifactProvider) => {
                formatter.write_str("test artifact providers are unavailable in production")
            }
            Self::IsolatedControlInProduction(IsolatedControl::TestCertificateProvider) => {
                formatter.write_str("test certificate providers are unavailable in production")
            }
            Self::IsolatedControlInProduction(IsolatedControl::TestCredential) => {
                formatter.write_str("test credentials are unavailable in production")
            }
        }
    }
}

impl Error for SecurityPolicyError {}

#[cfg(test)]
mod tests {
    use uob_contracts::Environment;

    use super::{IsolatedControl, RuntimeSecurityPolicy, SecurityPolicyError};

    #[test]
    fn production_rejects_every_isolated_control() {
        let policy = RuntimeSecurityPolicy::new(Environment::Production);

        for control in [
            IsolatedControl::Simulator,
            IsolatedControl::MockCheckout,
            IsolatedControl::TestAuthorizationProvider,
            IsolatedControl::TestArtifactProvider,
            IsolatedControl::TestCertificateProvider,
            IsolatedControl::TestCredential,
        ] {
            assert_eq!(
                policy.authorize_isolated_control(control),
                Err(SecurityPolicyError::IsolatedControlInProduction(control))
            );
        }
    }

    #[test]
    fn isolated_environments_allow_test_controls() {
        for environment in [Environment::Staging, Environment::Demo] {
            let policy = RuntimeSecurityPolicy::new(environment);
            assert!(
                policy
                    .authorize_isolated_control(IsolatedControl::Simulator)
                    .is_ok()
            );
            assert!(
                policy
                    .authorize_isolated_control(IsolatedControl::MockCheckout)
                    .is_ok()
            );
        }
    }

    #[test]
    fn production_rejects_test_authorization_provider_but_accepts_local_provider() {
        use crate::AuthorizationProviderDescriptor;

        let policy = RuntimeSecurityPolicy::new(Environment::Production);
        assert!(
            policy
                .authorize_authorization_provider(AuthorizationProviderDescriptor {
                    kind: "local.sha256",
                    test_only: false,
                })
                .is_ok()
        );
        assert_eq!(
            policy.authorize_authorization_provider(AuthorizationProviderDescriptor {
                kind: "test.accept-all",
                test_only: true,
            }),
            Err(SecurityPolicyError::IsolatedControlInProduction(
                IsolatedControl::TestAuthorizationProvider
            ))
        );
    }

    #[test]
    fn production_rejects_test_artifact_and_certificate_providers_and_material() {
        use crate::{
            artifact_provider::ArtifactProviderDescriptor,
            certificate_provider::CertificateProviderDescriptor,
        };

        let artifact = |test_only| ArtifactProviderDescriptor {
            kind: "test.artifacts",
            test_only,
        };
        let certificate = |test_only| CertificateProviderDescriptor {
            kind: "test.pki",
            test_only,
        };
        let production = RuntimeSecurityPolicy::new(Environment::Production);
        assert!(
            production
                .authorize_artifact_provider(artifact(false))
                .is_ok()
        );
        assert!(
            production
                .authorize_certificate_provider(certificate(false))
                .is_ok()
        );
        assert!(production.authorize_provider_material(false).is_ok());
        assert_eq!(
            production.authorize_artifact_provider(artifact(true)),
            Err(SecurityPolicyError::IsolatedControlInProduction(
                IsolatedControl::TestArtifactProvider
            ))
        );
        assert_eq!(
            production.authorize_certificate_provider(certificate(true)),
            Err(SecurityPolicyError::IsolatedControlInProduction(
                IsolatedControl::TestCertificateProvider
            ))
        );
        assert_eq!(
            production.authorize_provider_material(true),
            Err(SecurityPolicyError::IsolatedControlInProduction(
                IsolatedControl::TestCredential
            ))
        );
        for environment in [Environment::Staging, Environment::Demo] {
            let policy = RuntimeSecurityPolicy::new(environment);
            assert!(policy.authorize_artifact_provider(artifact(true)).is_ok());
            assert!(
                policy
                    .authorize_certificate_provider(certificate(true))
                    .is_ok()
            );
            assert!(policy.authorize_provider_material(true).is_ok());
        }
    }
}
