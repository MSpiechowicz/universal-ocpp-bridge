#![doc = "External export provider catalog and offline composition validation."]

mod catalog;
mod registry;
mod security;

pub use catalog::{
    DatabaseProviderCatalogEntry, DatabaseProviderRegistration, postgresql_configuration_schema,
};
pub use registry::{
    ConfiguredDatabaseProvider, DataExportConfiguration, DataExportSelectionError,
    DatabaseProviderRegistry, DatabaseRegistrationError, DestinationTransition, ExportBacklogState,
    POSTGRESQL_PROVIDER_KIND, ValidatedDataExport, ValidatedProviderSelection,
};
pub use security::{
    DatabaseSecurityError, DatabaseTransportSecurity, validate_database_transport_security,
};
