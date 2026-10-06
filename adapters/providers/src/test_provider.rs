//! Construction failures shared by the demo-only test providers.

use std::{error::Error, fmt};

use uob_application::SecurityPolicyError;

/// Sanitized test-provider failure without paths, keys or payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestProviderError {
    /// The runtime environment refuses test providers and their material.
    Policy(SecurityPolicyError),
    /// A configuration value or published artifact is invalid.
    InvalidConfiguration,
    /// The provider is at a configured bound.
    Capacity,
    /// Key generation or signing failed.
    Crypto,
}

impl From<SecurityPolicyError> for TestProviderError {
    fn from(error: SecurityPolicyError) -> Self {
        Self::Policy(error)
    }
}

impl fmt::Display for TestProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Policy(error) => write!(formatter, "test provider is refused: {error}"),
            Self::InvalidConfiguration => {
                formatter.write_str("test provider configuration is invalid")
            }
            Self::Capacity => formatter.write_str("test provider is at capacity"),
            Self::Crypto => formatter.write_str("test provider cryptography failed"),
        }
    }
}

impl Error for TestProviderError {}
