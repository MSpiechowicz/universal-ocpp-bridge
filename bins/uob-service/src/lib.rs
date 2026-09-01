#![doc = "Production service composition root."]

mod identity;

use std::{error::Error, fmt};

use uob_application::Application;
use uob_target_adapter::{
    BridgeTargetSelection, TargetRegistry, TargetSelectionError, ValidatedTargetSelection,
};

pub use identity::{StartupIdentityConfiguration, StartupIdentityError};

/// Fully composed service dependencies.
pub struct ServiceComposition<E, P> {
    /// Target kinds available in this service build.
    pub targets: TargetRegistry<E, P>,
    /// Target-neutral application facade.
    pub application: Application,
    /// Explicit selected target after offline registry and configuration validation.
    pub target_selection: Option<ValidatedTargetSelection<E, P>>,
}

/// Creates the service composition from trusted startup and optional target configuration.
///
/// # Errors
///
/// Rejects a selected target whose bridge or environment conflicts with the process identity.
pub fn compose<E, P>(
    targets: TargetRegistry<E, P>,
    configuration: StartupIdentityConfiguration,
    target_selection: Option<BridgeTargetSelection>,
) -> Result<ServiceComposition<E, P>, ServiceCompositionError> {
    let identity = identity::construct(configuration, target_selection.as_ref())?;
    let target_selection = target_selection
        .map(|selection| targets.validate(selection))
        .transpose()?;
    Ok(ServiceComposition {
        targets,
        application: Application::new(identity),
        target_selection,
    })
}

/// Sanitized service composition failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceCompositionError {
    /// Trusted service and selected-target identity conflict.
    Identity(StartupIdentityError),
    /// Selected target failed offline registry or configuration validation.
    Target(TargetSelectionError),
}

impl fmt::Display for ServiceCompositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Identity(source) => source.fmt(formatter),
            Self::Target(source) => source.fmt(formatter),
        }
    }
}

impl Error for ServiceCompositionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Identity(source) => Some(source),
            Self::Target(source) => Some(source),
        }
    }
}

impl From<StartupIdentityError> for ServiceCompositionError {
    fn from(source: StartupIdentityError) -> Self {
        Self::Identity(source)
    }
}

impl From<TargetSelectionError> for ServiceCompositionError {
    fn from(source: TargetSelectionError) -> Self {
        Self::Target(source)
    }
}

#[cfg(test)]
mod tests {
    use uob_contracts::{ArtifactDigest, BridgeId, Environment, ReleaseId, TargetInstanceId};
    use uob_target_adapter::{BridgeTargetSelection, TargetRegistry};

    use super::{
        ServiceCompositionError, StartupIdentityConfiguration, StartupIdentityError, compose,
    };

    fn startup() -> StartupIdentityConfiguration {
        StartupIdentityConfiguration::production(
            BridgeId::new("bridge-1").expect("bridge ID"),
            ReleaseId::new("release-1").expect("release ID"),
            ArtifactDigest::new("sha256:release-1").expect("release digest"),
        )
    }

    #[test]
    fn service_starts_without_constructing_an_unselected_target() {
        let service = compose(TargetRegistry::<(), ()>::new(), startup(), None)
            .expect("API-only composition");

        assert!(service.targets.is_empty());
        assert!(service.target_selection.is_none());
        assert!(service.application.identity().selected_target_id.is_none());
        assert_eq!(
            service.application.identity().runtime.environment,
            Environment::Production
        );
        assert_eq!(service.application.contract_version().major, 1);
    }

    #[test]
    fn restart_changes_only_process_identity() {
        let first =
            compose(TargetRegistry::<(), ()>::new(), startup(), None).expect("first composition");
        let second =
            compose(TargetRegistry::<(), ()>::new(), startup(), None).expect("second composition");

        assert_eq!(
            first.application.identity().bridge_id,
            second.application.identity().bridge_id
        );
        assert_eq!(
            first.application.identity().runtime.environment,
            second.application.identity().runtime.environment
        );
        assert_eq!(
            first.application.identity().runtime.release_id,
            second.application.identity().runtime.release_id
        );
        assert_ne!(
            first.application.identity().runtime.process_instance_id,
            second.application.identity().runtime.process_instance_id
        );
    }

    #[test]
    fn conflicting_target_environment_is_rejected() {
        let selection = BridgeTargetSelection {
            bridge_id: BridgeId::new("bridge-1").expect("bridge ID"),
            environment: Environment::Staging,
            target_id: TargetInstanceId::new("target-main").expect("target ID"),
            targets: Vec::new(),
        };

        let result = compose(TargetRegistry::<(), ()>::new(), startup(), Some(selection));

        assert!(matches!(
            result,
            Err(ServiceCompositionError::Identity(
                StartupIdentityError::EnvironmentMismatch
            ))
        ));
    }
}
