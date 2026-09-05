#![doc = "Production service composition root."]

pub mod cli;
mod configuration;
mod deployment;
mod event_stream;
mod identity;
mod lifecycle;
mod staging_network;
mod watchdog;

use std::{error::Error, fmt};

use uob_application::{Application, IsolatedControl, SecurityPolicyError};
use uob_external_export_adapter::{
    DataExportConfiguration, DataExportSelectionError, DatabaseProviderRegistry,
    DestinationTransition, ExportBacklogState, ValidatedDataExport,
};
use uob_target_adapter::{
    BridgeTargetSelection, TargetRegistry, TargetSelectionError, ValidatedTargetSelection,
};

pub use identity::{StartupIdentityConfiguration, StartupIdentityError};

/// Fully composed service dependencies.
pub struct ServiceComposition<E, P> {
    /// Target kinds available in this service build.
    pub targets: TargetRegistry<E, P>,
    /// External database kinds available in this service build.
    pub database_providers: DatabaseProviderRegistry,
    /// Target-neutral application facade.
    pub application: Application,
    /// Explicit selected target after offline registry and configuration validation.
    pub target_selection: Option<ValidatedTargetSelection<E, P>>,
    /// Optional provider selection after offline validation.
    pub data_export: ValidatedDataExport,
    /// Test-only controls validated against the trusted runtime environment.
    pub isolated_controls: IsolatedControlConfiguration,
}

/// Explicit simulator and mock-checkout endpoint configuration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IsolatedControlConfiguration {
    /// Whether simulator scenario and fault-injection endpoints are enabled.
    pub simulator: bool,
    /// Whether the local mock-checkout endpoint is enabled.
    pub mock_checkout: bool,
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
    compose_all(
        targets,
        DatabaseProviderRegistry::new(),
        configuration,
        target_selection,
        DataExportConfiguration::disabled(),
        &ExportBacklogState::default(),
        DestinationTransition::Preserve,
        IsolatedControlConfiguration::default(),
    )
}

/// Creates the service composition with optional external export configuration.
///
/// # Errors
///
/// Rejects unsafe provider configuration or a destination change that would reroute pending data.
pub fn compose_with_data_export<E, P>(
    targets: TargetRegistry<E, P>,
    database_providers: DatabaseProviderRegistry,
    configuration: StartupIdentityConfiguration,
    target_selection: Option<BridgeTargetSelection>,
    data_export: DataExportConfiguration,
    export_backlog: &ExportBacklogState,
    destination_transition: DestinationTransition,
) -> Result<ServiceComposition<E, P>, ServiceCompositionError> {
    compose_all(
        targets,
        database_providers,
        configuration,
        target_selection,
        data_export,
        export_backlog,
        destination_transition,
        IsolatedControlConfiguration::default(),
    )
}

/// Creates the service composition with explicitly requested isolated controls.
///
/// # Errors
///
/// Rejects test-only controls in production as well as identity and target conflicts.
pub fn compose_with_isolated_controls<E, P>(
    targets: TargetRegistry<E, P>,
    configuration: StartupIdentityConfiguration,
    target_selection: Option<BridgeTargetSelection>,
    isolated_controls: IsolatedControlConfiguration,
) -> Result<ServiceComposition<E, P>, ServiceCompositionError> {
    compose_all(
        targets,
        DatabaseProviderRegistry::new(),
        configuration,
        target_selection,
        DataExportConfiguration::disabled(),
        &ExportBacklogState::default(),
        DestinationTransition::Preserve,
        isolated_controls,
    )
}

#[allow(clippy::too_many_arguments)]
fn compose_all<E, P>(
    targets: TargetRegistry<E, P>,
    database_providers: DatabaseProviderRegistry,
    configuration: StartupIdentityConfiguration,
    target_selection: Option<BridgeTargetSelection>,
    data_export: DataExportConfiguration,
    export_backlog: &ExportBacklogState,
    destination_transition: DestinationTransition,
    isolated_controls: IsolatedControlConfiguration,
) -> Result<ServiceComposition<E, P>, ServiceCompositionError> {
    let identity = identity::construct(configuration, target_selection.as_ref())?;
    let data_export = database_providers.validate(
        identity.runtime.environment,
        data_export,
        export_backlog,
        destination_transition,
    )?;
    let application = Application::new(identity);
    if isolated_controls.simulator {
        application
            .security_policy()
            .authorize_isolated_control(IsolatedControl::Simulator)?;
    }
    if isolated_controls.mock_checkout {
        application
            .security_policy()
            .authorize_isolated_control(IsolatedControl::MockCheckout)?;
    }
    let target_selection = target_selection
        .map(|selection| targets.validate(selection))
        .transpose()?;
    Ok(ServiceComposition {
        targets,
        database_providers,
        application,
        target_selection,
        data_export,
        isolated_controls,
    })
}

/// Sanitized service composition failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceCompositionError {
    /// Trusted service and selected-target identity conflict.
    Identity(StartupIdentityError),
    /// Selected target failed offline registry or configuration validation.
    Target(TargetSelectionError),
    /// Optional external provider failed offline registry or destination validation.
    DataExport(DataExportSelectionError),
    /// Test-only endpoints conflict with the trusted runtime environment.
    Security(SecurityPolicyError),
}

impl fmt::Display for ServiceCompositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Identity(source) => source.fmt(formatter),
            Self::Target(source) => source.fmt(formatter),
            Self::DataExport(source) => source.fmt(formatter),
            Self::Security(source) => source.fmt(formatter),
        }
    }
}

impl Error for ServiceCompositionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Identity(source) => Some(source),
            Self::Target(source) => Some(source),
            Self::DataExport(source) => Some(source),
            Self::Security(source) => Some(source),
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

impl From<DataExportSelectionError> for ServiceCompositionError {
    fn from(source: DataExportSelectionError) -> Self {
        Self::DataExport(source)
    }
}

impl From<SecurityPolicyError> for ServiceCompositionError {
    fn from(source: SecurityPolicyError) -> Self {
        Self::Security(source)
    }
}

#[cfg(test)]
mod tests {
    use uob_application::{IsolatedControl, SecurityPolicyError};
    use uob_contracts::{ArtifactDigest, BridgeId, Environment, ReleaseId, TargetInstanceId};
    use uob_target_adapter::{BridgeTargetSelection, TargetRegistry};

    use super::{
        IsolatedControlConfiguration, ServiceCompositionError, StartupIdentityConfiguration,
        StartupIdentityError, compose, compose_with_isolated_controls,
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

    #[test]
    fn production_composition_rejects_simulator_and_mock_checkout_endpoints() {
        for controls in [
            IsolatedControlConfiguration {
                simulator: true,
                mock_checkout: false,
            },
            IsolatedControlConfiguration {
                simulator: false,
                mock_checkout: true,
            },
        ] {
            let result = compose_with_isolated_controls(
                TargetRegistry::<(), ()>::new(),
                startup(),
                None,
                controls,
            );

            assert!(matches!(
                result,
                Err(ServiceCompositionError::Security(
                    SecurityPolicyError::IsolatedControlInProduction(
                        IsolatedControl::Simulator | IsolatedControl::MockCheckout
                    )
                ))
            ));
        }
    }

    #[test]
    fn isolated_composition_retains_explicitly_enabled_controls() {
        let controls = IsolatedControlConfiguration {
            simulator: true,
            mock_checkout: true,
        };
        let service = compose_with_isolated_controls(
            TargetRegistry::<(), ()>::new(),
            startup().in_environment(Environment::Demo),
            None,
            controls,
        )
        .expect("isolated composition");

        assert_eq!(service.isolated_controls, controls);
    }
}
