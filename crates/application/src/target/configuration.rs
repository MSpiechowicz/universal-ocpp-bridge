use std::{collections::BTreeMap, error::Error, fmt};

use uob_contracts::TargetInstanceId;

use crate::DatabaseConfiguration;

/// Secret material is referenced by name or location and is not embedded in configuration.
#[derive(Clone, Eq, PartialEq)]
pub struct CredentialReference(String);

impl CredentialReference {
    /// Creates a non-empty credential reference.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigurationErrorCode::InvalidField`] for an empty reference.
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigurationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ConfigurationError::field(
                ConfigurationErrorCode::InvalidField,
                "credential_reference",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the reference for a credential provider to resolve at runtime.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CredentialReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialReference([redacted])")
    }
}

/// Typed configuration value accepted at the application-owned boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigurationValue {
    /// Non-secret text.
    Text(String),
    /// Signed integer.
    Integer(i64),
    /// Boolean flag.
    Boolean(bool),
    /// Reference resolved by the adapter only when the target starts.
    CredentialReference(CredentialReference),
}

/// Primitive kind declared by a target configuration schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationFieldKind {
    /// Non-secret text.
    Text,
    /// Signed integer.
    Integer,
    /// Boolean flag.
    Boolean,
    /// A credential reference, never credential contents.
    CredentialReference,
}

/// One safe field declaration in a target configuration schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationField {
    /// Stable field name.
    pub name: String,
    /// Accepted value kind.
    pub kind: ConfigurationFieldKind,
    /// Whether validation requires the field.
    pub required: bool,
}

/// Safe schema shown by management clients before any target is constructed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigurationSchema {
    /// Complete declared field list.
    pub fields: Vec<ConfigurationField>,
}

impl ConfigurationSchema {
    /// Validates required fields, declared names, and value kinds without resolving secrets or
    /// performing I/O.
    ///
    /// # Errors
    ///
    /// Returns a sanitized field error for the first missing, unknown, or incorrectly typed
    /// setting.
    pub fn validate_shape(
        &self,
        configuration: &TargetConfiguration,
    ) -> Result<(), ConfigurationError> {
        self.validate_settings(|name| configuration.setting(name), configuration.settings())
    }

    /// Validates the same safe schema against an external database configuration.
    ///
    /// # Errors
    ///
    /// Returns a sanitized field error for the first missing, unknown, or incorrectly typed
    /// setting.
    pub fn validate_database_shape(
        &self,
        configuration: &DatabaseConfiguration,
    ) -> Result<(), ConfigurationError> {
        self.validate_settings(|name| configuration.setting(name), configuration.settings())
    }

    fn validate_settings<'a>(
        &self,
        setting: impl Fn(&str) -> Option<&'a ConfigurationValue>,
        settings: impl Iterator<Item = (&'a str, &'a ConfigurationValue)>,
    ) -> Result<(), ConfigurationError> {
        for field in &self.fields {
            let value = setting(&field.name);
            if field.required && value.is_none() {
                return Err(ConfigurationError::field(
                    ConfigurationErrorCode::MissingField,
                    &field.name,
                ));
            }
            if let Some(value) = value {
                if !field.kind.accepts(value) {
                    return Err(ConfigurationError::field(
                        ConfigurationErrorCode::InvalidField,
                        &field.name,
                    ));
                }
            }
        }

        for (name, _) in settings {
            if !self.fields.iter().any(|field| field.name == name) {
                return Err(ConfigurationError::field(
                    ConfigurationErrorCode::UnknownField,
                    name,
                ));
            }
        }

        Ok(())
    }
}

impl ConfigurationFieldKind {
    const fn accepts(self, value: &ConfigurationValue) -> bool {
        matches!(
            (self, value),
            (Self::Text, ConfigurationValue::Text(_))
                | (Self::Integer, ConfigurationValue::Integer(_))
                | (Self::Boolean, ConfigurationValue::Boolean(_))
                | (
                    Self::CredentialReference,
                    ConfigurationValue::CredentialReference(_)
                )
        )
    }
}

/// Unvalidated settings for one configured target instance.
///
/// The type intentionally has no `Debug` implementation, avoiding accidental diagnostic output
/// of non-secret settings while they are being validated.
#[derive(Clone, Eq, PartialEq)]
pub struct TargetConfiguration {
    /// Stable configured instance.
    pub target_instance_id: TargetInstanceId,
    /// Immutable revision to which pending deliveries remain bound.
    pub revision: u64,
    settings: BTreeMap<String, ConfigurationValue>,
}

impl TargetConfiguration {
    /// Creates an unvalidated target configuration.
    #[must_use]
    pub const fn new(target_instance_id: TargetInstanceId, revision: u64) -> Self {
        Self {
            target_instance_id,
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

    /// Iterates over settings without exposing a driver-specific configuration type.
    pub fn settings(&self) -> impl Iterator<Item = (&str, &ConfigurationValue)> {
        self.settings
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }
}

/// Configuration proven acceptable by its matching factory.
///
/// Only a factory should construct this value, after validating every declared field. Creating it
/// performs no connection or credential lookup.
#[derive(Clone, Eq, PartialEq)]
pub struct ValidatedTargetConfiguration(TargetConfiguration);

impl ValidatedTargetConfiguration {
    /// Marks a fully checked configuration as validated.
    #[must_use]
    pub const fn new(configuration: TargetConfiguration) -> Self {
        Self(configuration)
    }

    /// Returns the validated configuration.
    #[must_use]
    pub const fn configuration(&self) -> &TargetConfiguration {
        &self.0
    }

    /// Consumes the proof wrapper.
    #[must_use]
    pub fn into_configuration(self) -> TargetConfiguration {
        self.0
    }
}

/// Stable configuration-validation failure classes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationErrorCode {
    /// A required field is absent.
    MissingField,
    /// A supplied field has the wrong type or violates a declared bound.
    InvalidField,
    /// A field is not declared by the schema.
    UnknownField,
    /// The requested configuration mode is not implemented by this target.
    Unsupported,
}

/// Sanitized configuration error that never retains a rejected value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationError {
    code: ConfigurationErrorCode,
    field: Option<String>,
}

impl ConfigurationError {
    /// Creates an error not tied to one field.
    #[must_use]
    pub const fn new(code: ConfigurationErrorCode) -> Self {
        Self { code, field: None }
    }

    /// Creates an error naming only the schema field, never its rejected value.
    #[must_use]
    pub fn field(code: ConfigurationErrorCode, field: impl Into<String>) -> Self {
        Self {
            code,
            field: Some(field.into()),
        }
    }

    /// Returns the stable error category.
    #[must_use]
    pub const fn code(&self) -> ConfigurationErrorCode {
        self.code
    }

    /// Returns the schema field involved, when applicable.
    #[must_use]
    pub fn field_name(&self) -> Option<&str> {
        self.field.as_deref()
    }
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.field {
            Some(field) => write!(formatter, "target configuration {:?} at {field}", self.code),
            None => write!(formatter, "target configuration {:?}", self.code),
        }
    }
}

impl Error for ConfigurationError {}
