use std::collections::BTreeMap;

use uob_contracts::ExportDestinationId;

use crate::ConfigurationValue;

/// Unvalidated settings for one external database destination.
///
/// This type intentionally omits `Debug`, preventing settings from being copied into diagnostics.
#[derive(Clone, Eq, PartialEq)]
pub struct DatabaseConfiguration {
    /// Stable provider instance and export destination identity.
    pub provider_instance_id: ExportDestinationId,
    /// Immutable configuration revision to which batches remain bound.
    pub revision: u64,
    settings: BTreeMap<String, ConfigurationValue>,
}

impl DatabaseConfiguration {
    /// Creates an empty unvalidated provider configuration.
    #[must_use]
    pub const fn new(provider_instance_id: ExportDestinationId, revision: u64) -> Self {
        Self {
            provider_instance_id,
            revision,
            settings: BTreeMap::new(),
        }
    }

    /// Adds or replaces one typed setting.
    #[must_use]
    pub fn with_setting(mut self, name: impl Into<String>, value: ConfigurationValue) -> Self {
        self.settings.insert(name.into(), value);
        self
    }

    /// Reads one setting during factory validation.
    #[must_use]
    pub fn setting(&self, name: &str) -> Option<&ConfigurationValue> {
        self.settings.get(name)
    }

    /// Iterates over typed settings without exposing a driver-specific configuration.
    pub fn settings(&self) -> impl Iterator<Item = (&str, &ConfigurationValue)> {
        self.settings
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }
}

/// Configuration proven acceptable by its matching factory without network I/O.
#[derive(Clone, Eq, PartialEq)]
pub struct ValidatedDatabaseConfiguration(DatabaseConfiguration);

impl ValidatedDatabaseConfiguration {
    /// Marks a fully checked provider configuration as validated.
    #[must_use]
    pub const fn new(configuration: DatabaseConfiguration) -> Self {
        Self(configuration)
    }

    /// Returns the validated configuration.
    #[must_use]
    pub const fn configuration(&self) -> &DatabaseConfiguration {
        &self.0
    }

    /// Consumes the proof wrapper.
    #[must_use]
    pub fn into_configuration(self) -> DatabaseConfiguration {
        self.0
    }
}
