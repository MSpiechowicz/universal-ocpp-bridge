use std::{collections::BTreeMap, error::Error, fmt, sync::Arc};

use uob_application::{
    ConfigurationError, ConfigurationSchema, DatabaseConfiguration, DatabaseProvider,
    DatabaseProviderFactory, ValidatedDatabaseConfiguration,
};
use uob_contracts::{Environment, ExportBatch, ExportDestination, ExportDestinationId};

use crate::{
    DatabaseProviderCatalogEntry, DatabaseProviderRegistration, DatabaseSecurityError,
    DatabaseTransportSecurity, validate_database_transport_security,
};

/// Reserved kind for the first external database provider.
pub const POSTGRESQL_PROVIDER_KIND: &str = "postgresql";

/// One configured external provider. The first release accepts at most one entry.
#[derive(Clone, Eq, PartialEq)]
pub struct ConfiguredDatabaseProvider {
    /// Stable registered provider kind.
    pub kind: String,
    /// Stable instance, revision, and typed driver settings.
    pub configuration: DatabaseConfiguration,
    /// Shared TLS and credential-reference facts.
    pub transport_security: DatabaseTransportSecurity,
}

/// Optional external-export startup configuration.
#[derive(Clone, Eq, PartialEq)]
pub struct DataExportConfiguration {
    /// Disabled export allocates no provider or runtime resources.
    pub enabled: bool,
    /// Explicit selected instance ID when export is enabled.
    pub provider_id: Option<ExportDestinationId>,
    /// Declared provider entries; at most one is supported in the first release.
    pub providers: Vec<ConfiguredDatabaseProvider>,
}

impl DataExportConfiguration {
    /// Returns the canonical disabled configuration.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            provider_id: None,
            providers: Vec::new(),
        }
    }
}

/// Durable host facts used to prevent pending data from changing destinations.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExportBacklogState {
    /// Destination and revision owning the pending checkpoint/batches, when present.
    pub destination: Option<ExportDestination>,
    /// Number of retained batches awaiting a terminal result.
    pub pending_batches: u64,
    /// Number of retained records awaiting a terminal result.
    pub pending_records: u64,
}

impl ExportBacklogState {
    fn is_pending(&self) -> bool {
        self.pending_batches != 0 || self.pending_records != 0
    }
}

/// Explicit operator-authorized transition for pending export data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DestinationTransition {
    /// Preserve all pending data and its immutable destination ownership.
    Preserve,
    /// Discard pending data through an already-authorized durable audit event.
    AuditedDiscard {
        /// Stable audit event proving who authorized the destructive transition.
        audit_event_id: String,
        /// Exact old destination the audit event authorized discarding.
        destination: ExportDestination,
    },
}

struct RegistryEntry {
    catalog: DatabaseProviderCatalogEntry,
    factory: Option<Arc<dyn DatabaseProviderFactory>>,
}

/// Provider registry populated only by the service composition root.
pub struct DatabaseProviderRegistry {
    entries: BTreeMap<String, RegistryEntry>,
}

impl Default for DatabaseProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DatabaseProviderRegistry {
    /// Creates an empty registry without constructing providers.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    /// Reserves `PostgreSQL` in the catalog until its concrete adapter is registered.
    ///
    /// # Errors
    ///
    /// Returns a duplicate-kind error if `PostgreSQL` was already registered.
    pub fn declare_postgresql_unavailable(
        &mut self,
        schema: ConfigurationSchema,
    ) -> Result<(), DatabaseRegistrationError> {
        self.declare_unavailable(
            POSTGRESQL_PROVIDER_KIND,
            schema,
            DatabaseProviderRegistration {
                display_name: "PostgreSQL".to_owned(),
            },
        )
    }

    /// Registers one implemented provider factory and its safe catalog metadata.
    ///
    /// # Errors
    ///
    /// Rejects an empty or duplicate kind.
    pub fn register(
        &mut self,
        factory: impl DatabaseProviderFactory + 'static,
        registration: DatabaseProviderRegistration,
    ) -> Result<(), DatabaseRegistrationError> {
        let kind = factory.kind();
        validate_kind(kind)?;
        if self.entries.contains_key(kind) {
            return Err(DatabaseRegistrationError::DuplicateKind(kind.to_owned()));
        }
        self.entries.insert(
            kind.to_owned(),
            RegistryEntry {
                catalog: DatabaseProviderCatalogEntry::new(
                    kind.to_owned(),
                    factory.configuration_schema(),
                    registration,
                    true,
                ),
                factory: Some(Arc::new(factory)),
            },
        );
        Ok(())
    }

    /// Declares a known provider kind that is unavailable in this executable.
    ///
    /// # Errors
    ///
    /// Rejects an empty or duplicate kind.
    pub fn declare_unavailable(
        &mut self,
        kind: &'static str,
        schema: ConfigurationSchema,
        registration: DatabaseProviderRegistration,
    ) -> Result<(), DatabaseRegistrationError> {
        validate_kind(kind)?;
        if self.entries.contains_key(kind) {
            return Err(DatabaseRegistrationError::DuplicateKind(kind.to_owned()));
        }
        self.entries.insert(
            kind.to_owned(),
            RegistryEntry {
                catalog: DatabaseProviderCatalogEntry::new(
                    kind.to_owned(),
                    schema,
                    registration,
                    false,
                ),
                factory: None,
            },
        );
        Ok(())
    }

    /// Returns deterministic credential-free provider metadata.
    #[must_use]
    pub fn catalog(&self) -> Vec<DatabaseProviderCatalogEntry> {
        self.entries
            .values()
            .map(|entry| entry.catalog.clone())
            .collect()
    }

    /// Returns the number of available provider factories.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.factory.is_some())
            .count()
    }

    /// Returns whether no provider factory is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Validates optional export without constructing or starting a provider.
    ///
    /// # Errors
    ///
    /// Rejects ambiguous/unknown configuration, unsafe transport, factory validation failures,
    /// and destination changes that do not first drain or explicitly audit-discard pending data.
    pub fn validate(
        &self,
        environment: Environment,
        configuration: DataExportConfiguration,
        backlog: &ExportBacklogState,
        transition: DestinationTransition,
    ) -> Result<ValidatedDataExport, DataExportSelectionError> {
        if !configuration.enabled {
            if configuration.provider_id.is_some() || !configuration.providers.is_empty() {
                return Err(DataExportSelectionError::DisabledWithProvider);
            }
            let discard_audit = validate_destination_transition(None, backlog, transition)?;
            return Ok(ValidatedDataExport::Disabled { discard_audit });
        }

        let provider_id = configuration
            .provider_id
            .ok_or(DataExportSelectionError::MissingProviderId)?;
        let [provider] = configuration.providers.as_slice() else {
            return Err(if configuration.providers.is_empty() {
                DataExportSelectionError::MissingProvider
            } else {
                DataExportSelectionError::MultipleProviders
            });
        };
        if provider.configuration.provider_instance_id != provider_id {
            return Err(DataExportSelectionError::ProviderIdMismatch);
        }
        let Some(entry) = self.entries.get(&provider.kind) else {
            return Err(DataExportSelectionError::UnknownKind(provider.kind.clone()));
        };
        let Some(factory) = entry.factory.clone() else {
            return Err(DataExportSelectionError::UnavailableKind(
                provider.kind.clone(),
            ));
        };
        validate_database_transport_security(environment, &provider.transport_security)?;
        entry
            .catalog
            .configuration_schema
            .validate_database_shape(&provider.configuration)
            .map_err(|source| DataExportSelectionError::InvalidConfiguration {
                kind: provider.kind.clone(),
                source,
            })?;
        let validated = factory
            .validate(&provider.configuration)
            .map_err(|source| DataExportSelectionError::InvalidConfiguration {
                kind: provider.kind.clone(),
                source,
            })?;
        let destination = ExportDestination {
            destination_id: provider_id,
            configuration_revision: validated.configuration().revision,
        };
        let discard_audit =
            validate_destination_transition(Some(&destination), backlog, transition)?;

        Ok(ValidatedDataExport::Enabled(ValidatedProviderSelection {
            catalog: entry.catalog.clone(),
            destination,
            discard_audit,
            configuration: validated,
            factory,
        }))
    }
}

fn validate_kind(kind: &str) -> Result<(), DatabaseRegistrationError> {
    if kind.trim().is_empty() {
        Err(DatabaseRegistrationError::InvalidKind)
    } else {
        Ok(())
    }
}

fn validate_destination_transition(
    next: Option<&ExportDestination>,
    backlog: &ExportBacklogState,
    transition: DestinationTransition,
) -> Result<Option<String>, DataExportSelectionError> {
    if !backlog.is_pending() || backlog.destination.as_ref() == next {
        return match transition {
            DestinationTransition::Preserve => Ok(None),
            DestinationTransition::AuditedDiscard { .. } => {
                Err(DataExportSelectionError::DiscardWithoutPendingChange)
            }
        };
    }
    let previous = backlog
        .destination
        .as_ref()
        .ok_or(DataExportSelectionError::PendingDestinationMissing)?;
    match transition {
        DestinationTransition::Preserve => Err(DataExportSelectionError::PendingDestinationChange),
        DestinationTransition::AuditedDiscard {
            audit_event_id,
            destination,
        } if !audit_event_id.trim().is_empty() && &destination == previous => {
            Ok(Some(audit_event_id))
        }
        DestinationTransition::AuditedDiscard { .. } => {
            Err(DataExportSelectionError::InvalidDiscardAudit)
        }
    }
}

/// Validated optional export state. Disabled has no factory or runtime allocation handle.
pub enum ValidatedDataExport {
    /// Export is disabled and cannot construct a provider.
    Disabled {
        /// Audit event authorizing discarded old pending data, when disabling required it.
        discard_audit: Option<String>,
    },
    /// Exactly one provider passed offline validation.
    Enabled(ValidatedProviderSelection),
}

impl ValidatedDataExport {
    /// Returns whether export is explicitly enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled(_))
    }

    /// Returns the immutable destination accepted for new batches.
    #[must_use]
    pub const fn destination(&self) -> Option<&ExportDestination> {
        match self {
            Self::Disabled { .. } => None,
            Self::Enabled(selection) => Some(&selection.destination),
        }
    }

    /// Rejects a pending batch owned by another instance or revision.
    ///
    /// # Errors
    ///
    /// Returns a destination-mismatch error rather than silently rerouting the batch.
    pub fn validate_batch_destination(
        &self,
        batch: &ExportBatch,
    ) -> Result<(), DataExportSelectionError> {
        if self.destination() == Some(batch.destination()) {
            Ok(())
        } else {
            Err(DataExportSelectionError::BatchDestinationMismatch)
        }
    }
}

/// One provider selection proven safe without I/O or construction.
pub struct ValidatedProviderSelection {
    /// Credential-free metadata from the same factory used for validation.
    pub catalog: DatabaseProviderCatalogEntry,
    /// Destination identity and revision to stamp onto all new batches/checkpoints.
    pub destination: ExportDestination,
    /// Audit event authorizing discarded old pending data, when used.
    pub discard_audit: Option<String>,
    configuration: ValidatedDatabaseConfiguration,
    factory: Arc<dyn DatabaseProviderFactory>,
}

impl ValidatedProviderSelection {
    /// Constructs the inactive provider after validation; this remains network-free.
    ///
    /// # Errors
    ///
    /// Returns a sanitized provider construction error.
    pub fn create(self) -> Result<Box<dyn DatabaseProvider>, ConfigurationError> {
        self.factory.create(self.configuration)
    }

    /// Returns the validated provider configuration.
    #[must_use]
    pub const fn configuration(&self) -> &ValidatedDatabaseConfiguration {
        &self.configuration
    }
}

/// Provider composition failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DatabaseRegistrationError {
    /// Provider kind was empty.
    InvalidKind,
    /// Kind was already registered or reserved.
    DuplicateKind(String),
}

/// Sanitized optional export selection failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataExportSelectionError {
    /// Disabled export included provider configuration that could allocate work accidentally.
    DisabledWithProvider,
    /// Enabled export omitted its selected ID.
    MissingProviderId,
    /// Enabled export omitted the provider entry.
    MissingProvider,
    /// The first release supports at most one external provider.
    MultipleProviders,
    /// Selected ID does not match the configured stable instance ID.
    ProviderIdMismatch,
    /// Provider kind is not recognized.
    UnknownKind(String),
    /// Provider kind is recognized but not implemented in this executable.
    UnavailableKind(String),
    /// Shared TLS or credential policy failed.
    InvalidTransportSecurity(DatabaseSecurityError),
    /// Schema or factory validation failed.
    InvalidConfiguration {
        /// Stable provider kind.
        kind: String,
        /// Sanitized field/category error.
        source: ConfigurationError,
    },
    /// Pending records have no durable destination owner.
    PendingDestinationMissing,
    /// Pending records must drain or be explicitly discarded before a destination change.
    PendingDestinationChange,
    /// Discard proof was empty or named a different old destination.
    InvalidDiscardAudit,
    /// Discard was requested even though no pending destination change exists.
    DiscardWithoutPendingChange,
    /// A queued batch belongs to another provider instance or revision.
    BatchDestinationMismatch,
}

impl From<DatabaseSecurityError> for DataExportSelectionError {
    fn from(source: DatabaseSecurityError) -> Self {
        Self::InvalidTransportSecurity(source)
    }
}

impl fmt::Display for DatabaseRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "database provider registration {self:?}")
    }
}

impl Error for DatabaseRegistrationError {}

impl fmt::Display for DataExportSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "external export selection {self:?}")
    }
}

impl Error for DataExportSelectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidTransportSecurity(source) => Some(source),
            Self::InvalidConfiguration { source, .. } => Some(source),
            _ => None,
        }
    }
}
