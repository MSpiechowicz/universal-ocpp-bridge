use uob_application::{ConfigurationField, ConfigurationFieldKind, ConfigurationSchema};

/// Safe first-release `PostgreSQL` configuration shape.
///
/// The schema deliberately exposes no connection string, password, raw SQL, or database command
/// field. Secret material can enter only through the typed credential-file reference.
#[must_use]
pub fn postgresql_configuration_schema() -> ConfigurationSchema {
    ConfigurationSchema {
        fields: vec![
            field("host", ConfigurationFieldKind::Text),
            field("port", ConfigurationFieldKind::Integer),
            field("database", ConfigurationFieldKind::Text),
            field("schema", ConfigurationFieldKind::Text),
            field(
                "credentials_file",
                ConfigurationFieldKind::CredentialReference,
            ),
            field("tls_mode", ConfigurationFieldKind::Text),
        ],
    }
}

fn field(name: &str, kind: ConfigurationFieldKind) -> ConfigurationField {
    ConfigurationField {
        name: name.to_owned(),
        kind,
        required: true,
    }
}

/// Credential-free metadata supplied beside a concrete database-provider factory.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DatabaseProviderRegistration {
    /// Human-readable provider name for configuration clients.
    pub display_name: String,
}

/// Safe provider entry shared by CLI, management, and browser configuration clients.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseProviderCatalogEntry {
    /// Stable factory kind, such as `postgresql`.
    pub kind: String,
    /// Human-readable provider name.
    pub display_name: String,
    /// Credential-free schema produced by the matching factory.
    pub configuration_schema: ConfigurationSchema,
    /// Whether this executable contains an implementation of the provider kind.
    pub available: bool,
}

impl DatabaseProviderCatalogEntry {
    pub(crate) fn new(
        kind: String,
        configuration_schema: ConfigurationSchema,
        registration: DatabaseProviderRegistration,
        available: bool,
    ) -> Self {
        Self {
            kind,
            display_name: registration.display_name,
            configuration_schema,
            available,
        }
    }
}
