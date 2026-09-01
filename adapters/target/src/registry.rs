use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    sync::Arc,
};

use uob_application::{
    BridgeTarget, BridgeTargetFactory, ConfigurationError, ConfigurationSchema,
    TargetConfiguration, ValidatedTargetConfiguration,
};
use uob_contracts::{BridgeId, Environment, TargetInstanceId, TargetKind};

use crate::{
    TargetCatalogEntry, TargetRegistration, TransportPolicyError, TransportSecurity,
    validate_transport_security,
};

/// One configured target. Only the enabled target is validated or constructed.
#[derive(Clone, Eq, PartialEq)]
pub struct ConfiguredTarget {
    /// Stable registered factory kind.
    pub kind: String,
    /// Whether this is the selected active target.
    pub enabled: bool,
    /// Instance identity, immutable revision, and driver settings.
    pub configuration: TargetConfiguration,
    /// Shared network-security facts, required only for network target kinds.
    pub transport_security: Option<TransportSecurity>,
}

/// Trusted bridge context and all declared target instances.
#[derive(Clone, Eq, PartialEq)]
pub struct BridgeTargetSelection {
    /// Stable bridge identity established by the composition root.
    pub bridge_id: BridgeId,
    /// Trusted runtime environment used by shared security validation.
    pub environment: Environment,
    /// Explicitly selected target instance.
    pub target_id: TargetInstanceId,
    /// Declared target instances; exactly one must be enabled.
    pub targets: Vec<ConfiguredTarget>,
}

struct RegistryEntry<E, P> {
    catalog: TargetCatalogEntry,
    factory: Option<Arc<dyn BridgeTargetFactory<E, P>>>,
}

/// Registry populated exclusively by the service composition root.
pub struct TargetRegistry<E, P> {
    entries: BTreeMap<String, RegistryEntry<E, P>>,
}

impl<E, P> Default for TargetRegistry<E, P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E, P> TargetRegistry<E, P> {
    /// Creates an empty registry without constructing adapters.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    /// Registers one implemented target factory and its safe catalog metadata.
    ///
    /// # Errors
    ///
    /// Rejects empty or duplicate stable kinds.
    pub fn register(
        &mut self,
        factory: impl BridgeTargetFactory<E, P> + 'static,
        registration: TargetRegistration,
    ) -> Result<(), RegistrationError> {
        let kind = factory.kind();
        let target_kind = TargetKind::new(kind).map_err(|_| RegistrationError::InvalidKind)?;
        if self.entries.contains_key(kind) {
            return Err(RegistrationError::DuplicateKind(kind.to_owned()));
        }
        let schema = factory.configuration_schema();
        self.entries.insert(
            kind.to_owned(),
            RegistryEntry {
                catalog: TargetCatalogEntry::new(target_kind, schema, registration, true),
                factory: Some(Arc::new(factory)),
            },
        );
        Ok(())
    }

    /// Declares a recognized but unavailable target kind for explicit diagnostics.
    ///
    /// # Errors
    ///
    /// Rejects empty or duplicate stable kinds.
    pub fn declare_unavailable(
        &mut self,
        kind: &'static str,
        schema: ConfigurationSchema,
        registration: TargetRegistration,
    ) -> Result<(), RegistrationError> {
        let target_kind = TargetKind::new(kind).map_err(|_| RegistrationError::InvalidKind)?;
        if self.entries.contains_key(kind) {
            return Err(RegistrationError::DuplicateKind(kind.to_owned()));
        }
        self.entries.insert(
            kind.to_owned(),
            RegistryEntry {
                catalog: TargetCatalogEntry::new(target_kind, schema, registration, false),
                factory: None,
            },
        );
        Ok(())
    }

    /// Returns the deterministic credential-free catalog used by all management clients.
    #[must_use]
    pub fn catalog(&self) -> Vec<TargetCatalogEntry> {
        self.entries
            .values()
            .map(|entry| entry.catalog.clone())
            .collect()
    }

    /// Returns whether an implemented target factory is available.
    #[must_use]
    pub fn contains(&self, kind: &str) -> bool {
        self.entries
            .get(kind)
            .is_some_and(|entry| entry.factory.is_some())
    }

    /// Returns the number of implemented target factories.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.factory.is_some())
            .count()
    }

    /// Returns whether no implemented target factories are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Validates one explicit selection without constructing a target, resolving credentials,
    /// or performing network I/O.
    ///
    /// # Errors
    ///
    /// Rejects duplicate instances, unknown/unavailable kinds, ambiguous selection, schema
    /// violations, and factory-specific configuration errors.
    pub fn validate(
        &self,
        selection: BridgeTargetSelection,
    ) -> Result<ValidatedTargetSelection<E, P>, TargetSelectionError> {
        let mut instance_ids = BTreeSet::new();
        for target in &selection.targets {
            let instance_id = target.configuration.target_instance_id.as_str();
            if !instance_ids.insert(instance_id) {
                return Err(TargetSelectionError::DuplicateInstanceId(
                    instance_id.to_owned(),
                ));
            }
            let Some(entry) = self.entries.get(&target.kind) else {
                return Err(TargetSelectionError::UnknownKind(target.kind.clone()));
            };
            if entry.factory.is_none() {
                return Err(TargetSelectionError::UnavailableKind(target.kind.clone()));
            }
        }

        let target = active_target(&selection)?;

        let Some(entry) = self.entries.get(&target.kind) else {
            return Err(TargetSelectionError::UnknownKind(target.kind.clone()));
        };
        if let Some(policy) = entry.catalog.transport_policy {
            let security = target
                .transport_security
                .as_ref()
                .ok_or(TargetSelectionError::MissingTransportSecurity)?;
            validate_transport_security(selection.environment, policy, security)
                .map_err(TargetSelectionError::InvalidTransportSecurity)?;
        }
        entry
            .catalog
            .configuration_schema
            .validate_shape(&target.configuration)
            .map_err(|source| TargetSelectionError::InvalidConfiguration {
                kind: target.kind.clone(),
                source,
            })?;
        let Some(factory) = entry.factory.clone() else {
            return Err(TargetSelectionError::UnavailableKind(target.kind.clone()));
        };
        let configuration = factory.validate(&target.configuration).map_err(|source| {
            TargetSelectionError::InvalidConfiguration {
                kind: target.kind.clone(),
                source,
            }
        })?;

        Ok(ValidatedTargetSelection {
            bridge_id: selection.bridge_id,
            environment: selection.environment,
            target_id: selection.target_id,
            catalog: entry.catalog.clone(),
            configuration,
            factory,
        })
    }
}

fn active_target(
    selection: &BridgeTargetSelection,
) -> Result<&ConfiguredTarget, TargetSelectionError> {
    let mut active = selection.targets.iter().filter(|target| target.enabled);
    let Some(target) = active.next() else {
        return Err(TargetSelectionError::MissingActiveTarget);
    };
    if active.next().is_some() {
        return Err(TargetSelectionError::MultipleActiveTargets);
    }
    if target.configuration.target_instance_id != selection.target_id {
        return Err(TargetSelectionError::TargetIdMismatch);
    }
    Ok(target)
}

/// One selection proven unambiguous and accepted by the matching factory.
pub struct ValidatedTargetSelection<E, P> {
    /// Trusted bridge identity.
    pub bridge_id: BridgeId,
    /// Trusted runtime environment.
    pub environment: Environment,
    /// Stable selected target instance.
    pub target_id: TargetInstanceId,
    /// Safe metadata from the same factory used for validation.
    pub catalog: TargetCatalogEntry,
    configuration: ValidatedTargetConfiguration,
    factory: Arc<dyn BridgeTargetFactory<E, P>>,
}

impl<E: 'static, P: 'static> ValidatedTargetSelection<E, P> {
    /// Explicitly constructs the inactive adapter after validation.
    ///
    /// # Errors
    ///
    /// Returns a sanitized factory construction error. Construction remains network-free by the
    /// factory contract; starting the returned adapter is a separate operation.
    pub fn create(self) -> Result<Box<dyn BridgeTarget<E, P>>, ConfigurationError> {
        self.factory.create(self.configuration)
    }

    /// Returns the validated driver configuration without exposing rejected values.
    #[must_use]
    pub const fn configuration(&self) -> &ValidatedTargetConfiguration {
        &self.configuration
    }
}

/// Composition failures while registering target kinds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistrationError {
    /// Factory kind was empty.
    InvalidKind,
    /// Kind was already registered or declared.
    DuplicateKind(String),
}

/// Sanitized shared target-selection failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetSelectionError {
    /// Two configured targets reuse one stable instance ID.
    DuplicateInstanceId(String),
    /// No target is enabled.
    MissingActiveTarget,
    /// More than one target is enabled.
    MultipleActiveTargets,
    /// `bridge.target_id` does not name the enabled instance.
    TargetIdMismatch,
    /// The selected network target omitted shared transport-security facts.
    MissingTransportSecurity,
    /// The selected target violates shared environment/transport policy.
    InvalidTransportSecurity(TransportPolicyError),
    /// Kind is not recognized by this composition root.
    UnknownKind(String),
    /// Kind is recognized but has no factory in this executable.
    UnavailableKind(String),
    /// Shared schema or matching factory rejected settings.
    InvalidConfiguration {
        /// Stable target kind.
        kind: String,
        /// Sanitized field/category error.
        source: ConfigurationError,
    },
}

impl fmt::Display for TargetSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateInstanceId(id) => write!(formatter, "duplicate target instance {id}"),
            Self::MissingActiveTarget => formatter.write_str("no active target configured"),
            Self::MultipleActiveTargets => {
                formatter.write_str("multiple active targets configured")
            }
            Self::TargetIdMismatch => {
                formatter.write_str("bridge target ID does not select the active target")
            }
            Self::MissingTransportSecurity => {
                formatter.write_str("active network target is missing transport security")
            }
            Self::InvalidTransportSecurity(source) => write!(formatter, "{source}"),
            Self::UnknownKind(kind) => write!(formatter, "unknown target kind {kind}"),
            Self::UnavailableKind(kind) => write!(formatter, "unavailable target kind {kind}"),
            Self::InvalidConfiguration { kind, source } => {
                write!(formatter, "invalid configuration for {kind}: {source}")
            }
        }
    }
}

impl Error for TargetSelectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidConfiguration { source, .. } => Some(source),
            Self::InvalidTransportSecurity(source) => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use uob_application::{ConfigurationSchema, TargetConfiguration};
    use uob_contracts::{BridgeId, Environment, TargetInstanceId};

    use super::{
        BridgeTargetSelection, ConfiguredTarget, RegistrationError, TargetRegistry,
        TargetSelectionError, active_target,
    };
    use crate::{TargetDisplayFamily, TargetRegistration};

    fn target_id(value: &str) -> TargetInstanceId {
        TargetInstanceId::new(value).expect("target ID")
    }

    #[test]
    fn bridge_target_id_must_name_the_single_enabled_instance() {
        let selection = BridgeTargetSelection {
            bridge_id: BridgeId::new("bridge-1").expect("bridge ID"),
            environment: Environment::Demo,
            target_id: target_id("selected"),
            targets: vec![ConfiguredTarget {
                kind: "test.memory".to_owned(),
                enabled: true,
                configuration: TargetConfiguration::new(target_id("different"), 1),
                transport_security: None,
            }],
        };

        assert!(matches!(
            active_target(&selection),
            Err(TargetSelectionError::TargetIdMismatch)
        ));
    }

    #[test]
    fn duplicate_declared_kinds_are_rejected() {
        let mut registry = TargetRegistry::<(), ()>::new();
        let registration = || TargetRegistration {
            display_family: TargetDisplayFamily {
                id: "test".to_owned(),
                display_name: "Test".to_owned(),
            },
            presets: vec![],
            capabilities: vec![],
            transport_policy: None,
        };
        registry
            .declare_unavailable(
                "future.test",
                ConfigurationSchema::default(),
                registration(),
            )
            .expect("first declaration");

        assert_eq!(
            registry.declare_unavailable(
                "future.test",
                ConfigurationSchema::default(),
                registration()
            ),
            Err(RegistrationError::DuplicateKind("future.test".to_owned()))
        );
    }
}
