//! Runtime security gates for isolated controls and payment evidence.

use std::{error::Error, fmt};

use uob_contracts::Environment;

/// Controls that are valid only in an explicitly isolated environment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IsolatedControl {
    /// Simulator scenario or fault-injection endpoint.
    Simulator,
    /// Local mock-checkout endpoint.
    MockCheckout,
}

/// Opaque safe reference to authorization evidence owned by a payment provider.
#[derive(Clone, Eq, PartialEq)]
pub struct ProviderAuthorizationReference(String);

impl ProviderAuthorizationReference {
    /// Creates a non-empty bounded provider reference.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderAuthorizationReferenceError`] for empty or oversized values.
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderAuthorizationReferenceError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ProviderAuthorizationReferenceError::Empty);
        }
        if value.len() > 256 {
            return Err(ProviderAuthorizationReferenceError::TooLong);
        }
        Ok(Self(value))
    }

    /// Returns the provider-owned authorization reference.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ProviderAuthorizationReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderAuthorizationReference([OPAQUE])")
    }
}

/// Invalid provider authorization reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderAuthorizationReferenceError {
    /// No reference was supplied.
    Empty,
    /// The reference exceeds the application boundary.
    TooLong,
}

impl fmt::Display for ProviderAuthorizationReferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "provider authorization reference cannot be empty",
            Self::TooLong => "provider authorization reference is too long",
        })
    }
}

impl Error for ProviderAuthorizationReferenceError {}

/// Payment evidence accepted at the application boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PaymentAuthorizationEvidence {
    /// Verification supplied by the configured provider interface.
    ProviderVerified(ProviderAuthorizationReference),
    /// Untrusted browser claim, regardless of its Boolean value.
    BrowserAssertion(bool),
}

/// Verified evidence which can be consumed by payment-dependent application logic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedPaymentAuthorization(ProviderAuthorizationReference);

impl VerifiedPaymentAuthorization {
    /// Returns the opaque provider reference.
    #[must_use]
    pub fn provider_reference(&self) -> &ProviderAuthorizationReference {
        &self.0
    }
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

    /// Accepts provider verification and rejects every browser assertion.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityPolicyError::UnverifiedBrowserPayment`] for browser claims.
    pub fn verify_payment(
        self,
        evidence: PaymentAuthorizationEvidence,
    ) -> Result<VerifiedPaymentAuthorization, SecurityPolicyError> {
        match evidence {
            PaymentAuthorizationEvidence::ProviderVerified(reference) => {
                Ok(VerifiedPaymentAuthorization(reference))
            }
            PaymentAuthorizationEvidence::BrowserAssertion(_) => {
                Err(SecurityPolicyError::UnverifiedBrowserPayment)
            }
        }
    }
}

/// Stable policy rejection safe for diagnostics and API errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityPolicyError {
    /// A test-only endpoint was requested in production.
    IsolatedControlInProduction(IsolatedControl),
    /// A browser claimed payment success without provider verification.
    UnverifiedBrowserPayment,
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
            Self::UnverifiedBrowserPayment => {
                formatter.write_str("payment authorization requires provider verification")
            }
        }
    }
}

impl Error for SecurityPolicyError {}

#[cfg(test)]
mod tests {
    use uob_contracts::Environment;

    use super::{
        IsolatedControl, PaymentAuthorizationEvidence, ProviderAuthorizationReference,
        RuntimeSecurityPolicy, SecurityPolicyError,
    };

    #[test]
    fn production_rejects_every_isolated_control() {
        let policy = RuntimeSecurityPolicy::new(Environment::Production);

        for control in [IsolatedControl::Simulator, IsolatedControl::MockCheckout] {
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
    fn browser_payment_success_is_never_authorization() {
        let policy = RuntimeSecurityPolicy::new(Environment::Demo);

        assert_eq!(
            policy.verify_payment(PaymentAuthorizationEvidence::BrowserAssertion(true)),
            Err(SecurityPolicyError::UnverifiedBrowserPayment)
        );
        assert_eq!(
            policy.verify_payment(PaymentAuthorizationEvidence::BrowserAssertion(false)),
            Err(SecurityPolicyError::UnverifiedBrowserPayment)
        );
    }

    #[test]
    fn only_provider_verification_produces_authorized_evidence() {
        let reference =
            ProviderAuthorizationReference::new("provider-auth-42").expect("provider reference");
        let verified = RuntimeSecurityPolicy::new(Environment::Production)
            .verify_payment(PaymentAuthorizationEvidence::ProviderVerified(
                reference.clone(),
            ))
            .expect("provider verification");

        assert_eq!(verified.provider_reference(), &reference);
        assert!(!format!("{verified:?}").contains(reference.as_str()));
    }
}
