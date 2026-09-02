//! Runtime security gates for isolated controls.

use std::{error::Error, fmt};

use uob_contracts::Environment;

use crate::AuthorizationProviderDescriptor;

/// Controls that are valid only in an explicitly isolated environment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IsolatedControl {
    /// Simulator scenario or fault-injection endpoint.
    Simulator,
    /// Local mock-checkout endpoint.
    MockCheckout,
    /// Authorization provider that accepts test credentials.
    TestAuthorizationProvider,
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
}
