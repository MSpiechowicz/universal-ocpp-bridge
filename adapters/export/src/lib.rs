#![doc = "External export provider catalog, validation, and optional delivery supervision."]

mod catalog;
mod registry;
mod scheduler;
mod security;

pub use catalog::{
    DatabaseProviderCatalogEntry, DatabaseProviderRegistration, postgresql_configuration_schema,
};
pub use registry::{
    ConfiguredDatabaseProvider, DataExportConfiguration, DataExportSelectionError,
    DatabaseProviderRegistry, DatabaseRegistrationError, DestinationTransition, ExportBacklogState,
    POSTGRESQL_PROVIDER_KIND, ValidatedDataExport, ValidatedProviderSelection,
};
pub use scheduler::{
    ExportScheduler, ExportSchedulerError, ExportSchedulerHandle, ExportSchedulerHealth,
};
pub use security::{
    DatabaseSecurityError, DatabaseTransportSecurity, validate_database_transport_security,
};
