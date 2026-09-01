use std::{error::Error, fmt};

use uob_contracts::{
    ArtifactDigest, BridgeId, Environment, ProcessInstanceId, ReleaseId, RuntimeIdentity,
    ServiceIdentity,
};
use uob_target_adapter::BridgeTargetSelection;
use uuid::Uuid;

/// Trusted startup configuration loaded by the service composition root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupIdentityConfiguration {
    /// Stable bridge installation identity.
    pub bridge_id: BridgeId,
    /// Explicit environment; production is the constructor default.
    pub environment: Environment,
    /// Immutable release identity from the installed artifact.
    pub release_id: ReleaseId,
    /// Digest verified for the installed artifact.
    pub release_digest: ArtifactDigest,
}

impl StartupIdentityConfiguration {
    /// Creates the default production identity configuration.
    #[must_use]
    pub const fn production(
        bridge_id: BridgeId,
        release_id: ReleaseId,
        release_digest: ArtifactDigest,
    ) -> Self {
        Self {
            bridge_id,
            environment: Environment::Production,
            release_id,
            release_digest,
        }
    }

    /// Overrides the environment for an explicitly isolated staging or demo instance.
    #[must_use]
    pub const fn in_environment(mut self, environment: Environment) -> Self {
        self.environment = environment;
        self
    }
}

/// Builds trusted service identity and rejects conflicting target configuration.
pub(crate) fn construct(
    configuration: StartupIdentityConfiguration,
    target: Option<&BridgeTargetSelection>,
) -> Result<ServiceIdentity, StartupIdentityError> {
    let selected_target_id = if let Some(target) = target {
        if target.bridge_id != configuration.bridge_id {
            return Err(StartupIdentityError::BridgeMismatch);
        }
        if target.environment != configuration.environment {
            return Err(StartupIdentityError::EnvironmentMismatch);
        }
        Some(target.target_id.clone())
    } else {
        None
    };

    let process_instance_id = ProcessInstanceId::new(Uuid::new_v4().hyphenated().to_string())
        .expect("UUID process identity is never empty");

    Ok(ServiceIdentity {
        bridge_id: configuration.bridge_id,
        runtime: RuntimeIdentity {
            environment: configuration.environment,
            release_id: configuration.release_id,
            release_digest: configuration.release_digest,
            process_instance_id,
        },
        selected_target_id,
    })
}

/// Trusted startup identity conflicts with selected-target configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupIdentityError {
    /// Target configuration names another bridge installation.
    BridgeMismatch,
    /// Target configuration names another runtime environment.
    EnvironmentMismatch,
}

impl fmt::Display for StartupIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BridgeMismatch => {
                formatter.write_str("selected target bridge does not match service identity")
            }
            Self::EnvironmentMismatch => {
                formatter.write_str("selected target environment does not match service identity")
            }
        }
    }
}

impl Error for StartupIdentityError {}
