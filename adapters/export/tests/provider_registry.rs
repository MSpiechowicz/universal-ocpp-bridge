use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use uob_application::{
    ConfigurationError, ConfigurationErrorCode, ConfigurationField, ConfigurationFieldKind,
    ConfigurationSchema, ConfigurationValue, CredentialReference, DatabaseConfiguration,
    DatabaseProvider, DatabaseProviderFactory, ValidatedDatabaseConfiguration,
};
use uob_contracts::{Environment, ExportBatch, ExportDestination, ExportDestinationId};
use uob_external_export_adapter::{
    ConfiguredDatabaseProvider, DataExportConfiguration, DataExportSelectionError,
    DatabaseProviderRegistration, DatabaseProviderRegistry, DatabaseSecurityError,
    DatabaseTransportSecurity, DestinationTransition, ExportBacklogState, ValidatedDataExport,
    postgresql_configuration_schema,
};

#[derive(Default)]
struct FactoryCalls {
    validated: AtomicUsize,
    created: AtomicUsize,
}

struct PostgresqlFixtureFactory {
    calls: Arc<FactoryCalls>,
}

struct ArchiveFixtureFactory;

impl DatabaseProviderFactory for ArchiveFixtureFactory {
    fn kind(&self) -> &'static str {
        "test.archive"
    }

    fn configuration_schema(&self) -> ConfigurationSchema {
        ConfigurationSchema {
            fields: vec![ConfigurationField {
                name: "credentials_file".to_owned(),
                kind: ConfigurationFieldKind::CredentialReference,
                required: true,
            }],
        }
    }

    fn validate(
        &self,
        configuration: &DatabaseConfiguration,
    ) -> Result<ValidatedDatabaseConfiguration, ConfigurationError> {
        Ok(ValidatedDatabaseConfiguration::new(configuration.clone()))
    }

    fn create(
        &self,
        _configuration: ValidatedDatabaseConfiguration,
    ) -> Result<Box<dyn DatabaseProvider>, ConfigurationError> {
        Err(ConfigurationError::new(ConfigurationErrorCode::Unsupported))
    }
}

impl DatabaseProviderFactory for PostgresqlFixtureFactory {
    fn kind(&self) -> &'static str {
        "postgresql"
    }

    fn configuration_schema(&self) -> uob_application::ConfigurationSchema {
        postgresql_configuration_schema()
    }

    fn validate(
        &self,
        configuration: &DatabaseConfiguration,
    ) -> Result<ValidatedDatabaseConfiguration, ConfigurationError> {
        self.calls.validated.fetch_add(1, Ordering::SeqCst);
        for (field, required) in [("schema", "uob"), ("tls_mode", "verify-full")] {
            if configuration.setting(field) != Some(&ConfigurationValue::Text(required.to_owned()))
            {
                return Err(ConfigurationError::field(
                    ConfigurationErrorCode::InvalidField,
                    field,
                ));
            }
        }
        Ok(ValidatedDatabaseConfiguration::new(configuration.clone()))
    }

    fn create(
        &self,
        _configuration: ValidatedDatabaseConfiguration,
    ) -> Result<Box<dyn DatabaseProvider>, ConfigurationError> {
        self.calls.created.fetch_add(1, Ordering::SeqCst);
        Err(ConfigurationError::new(ConfigurationErrorCode::Unsupported))
    }
}

fn destination(value: &str) -> ExportDestinationId {
    ExportDestinationId::new(value).expect("valid fixture destination")
}

fn credentials() -> CredentialReference {
    CredentialReference::new("/etc/uob/production/secrets/postgresql.toml")
        .expect("credential reference")
}

fn configuration(id: &str, revision: u64) -> DatabaseConfiguration {
    DatabaseConfiguration::new(destination(id), revision)
        .with_setting(
            "host",
            ConfigurationValue::Text("database.example".to_owned()),
        )
        .with_setting("port", ConfigurationValue::Integer(5432))
        .with_setting(
            "database",
            ConfigurationValue::Text("charging_production".to_owned()),
        )
        .with_setting("schema", ConfigurationValue::Text("uob".to_owned()))
        .with_setting(
            "credentials_file",
            ConfigurationValue::CredentialReference(credentials()),
        )
        .with_setting(
            "tls_mode",
            ConfigurationValue::Text("verify-full".to_owned()),
        )
}

fn secure_transport() -> DatabaseTransportSecurity {
    DatabaseTransportSecurity {
        tls: true,
        certificate_verification: true,
        credentials_file: Some(credentials()),
        explicitly_isolated: false,
    }
}

fn enabled(id: &str, revision: u64) -> DataExportConfiguration {
    DataExportConfiguration {
        enabled: true,
        provider_id: Some(destination(id)),
        providers: vec![ConfiguredDatabaseProvider {
            kind: "postgresql".to_owned(),
            configuration: configuration(id, revision),
            transport_security: secure_transport(),
        }],
    }
}

fn registry(calls: Arc<FactoryCalls>) -> DatabaseProviderRegistry {
    let mut registry = DatabaseProviderRegistry::new();
    registry
        .register(
            PostgresqlFixtureFactory { calls },
            DatabaseProviderRegistration {
                display_name: "PostgreSQL".to_owned(),
            },
        )
        .expect("register PostgreSQL fixture");
    registry
}

fn validate(
    registry: &DatabaseProviderRegistry,
    configuration: DataExportConfiguration,
) -> Result<ValidatedDataExport, DataExportSelectionError> {
    registry.validate(
        Environment::Production,
        configuration,
        &ExportBacklogState::default(),
        DestinationTransition::Preserve,
    )
}

#[test]
fn disabled_export_has_no_factory_or_runtime_allocation_handle() {
    let calls = Arc::new(FactoryCalls::default());
    let registry = registry(Arc::clone(&calls));

    let selection =
        validate(&registry, DataExportConfiguration::disabled()).expect("disabled export is valid");

    assert!(!selection.is_enabled());
    assert!(selection.destination().is_none());
    assert_eq!(calls.validated.load(Ordering::SeqCst), 0);
    assert_eq!(calls.created.load(Ordering::SeqCst), 0);
    assert!(matches!(
        selection,
        ValidatedDataExport::Disabled {
            discard_audit: None
        }
    ));
}

#[test]
fn postgresql_is_independent_of_every_first_release_target_mode() {
    for target_mode in ["mqtt", "ems-scada.http", "mqtt:ems-scada"] {
        let calls = Arc::new(FactoryCalls::default());
        let registry = registry(Arc::clone(&calls));
        let selection = validate(&registry, enabled("analytics", 4))
            .unwrap_or_else(|error| panic!("export failed beside {target_mode}: {error}"));

        assert_eq!(
            selection.destination(),
            Some(&ExportDestination {
                destination_id: destination("analytics"),
                configuration_revision: 4,
            })
        );
        assert_eq!(calls.validated.load(Ordering::SeqCst), 1);
        assert_eq!(calls.created.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn replacement_provider_is_added_only_through_schema_and_factory_registration() {
    let calls = Arc::new(FactoryCalls::default());
    let mut registry = registry(Arc::clone(&calls));
    registry
        .register(
            ArchiveFixtureFactory,
            DatabaseProviderRegistration {
                display_name: "Archive fixture".to_owned(),
            },
        )
        .expect("register replacement provider");
    let replacement = DataExportConfiguration {
        enabled: true,
        provider_id: Some(destination("archive")),
        providers: vec![ConfiguredDatabaseProvider {
            kind: "test.archive".to_owned(),
            configuration: DatabaseConfiguration::new(destination("archive"), 3).with_setting(
                "credentials_file",
                ConfigurationValue::CredentialReference(credentials()),
            ),
            transport_security: secure_transport(),
        }],
    };

    let selection = validate(&registry, replacement).expect("replacement validates offline");
    assert_eq!(registry.len(), 2);
    assert_eq!(
        selection.destination(),
        Some(&ExportDestination {
            destination_id: destination("archive"),
            configuration_revision: 3,
        })
    );
    assert_eq!(calls.validated.load(Ordering::SeqCst), 0);
    assert_eq!(calls.created.load(Ordering::SeqCst), 0);
}

#[test]
fn catalog_is_credential_free_and_reserves_unavailable_postgresql() {
    let mut registry = DatabaseProviderRegistry::new();
    registry
        .declare_postgresql_unavailable(postgresql_configuration_schema())
        .expect("reserve PostgreSQL");

    let catalog = registry.catalog();
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].kind, "postgresql");
    assert!(!catalog[0].available);
    assert!(
        catalog[0]
            .configuration_schema
            .fields
            .iter()
            .any(|field| field.name == "credentials_file")
    );
    assert!(
        !catalog[0]
            .configuration_schema
            .fields
            .iter()
            .any(|field| matches!(
                field.name.as_str(),
                "password" | "sql" | "connection_string"
            ))
    );
}

#[test]
fn ambiguous_unknown_and_disabled_provider_configuration_is_rejected() {
    let registry = registry(Arc::new(FactoryCalls::default()));

    let mut multiple = enabled("analytics", 1);
    multiple.providers.push(ConfiguredDatabaseProvider {
        kind: "postgresql".to_owned(),
        configuration: configuration("second", 1),
        transport_security: secure_transport(),
    });
    assert!(matches!(
        validate(&registry, multiple),
        Err(DataExportSelectionError::MultipleProviders)
    ));

    let mut unknown = enabled("analytics", 1);
    unknown.providers[0].kind = "unknown".to_owned();
    assert!(matches!(
        validate(&registry, unknown),
        Err(DataExportSelectionError::UnknownKind(kind)) if kind == "unknown"
    ));

    let mut disabled_with_provider = enabled("analytics", 1);
    disabled_with_provider.enabled = false;
    assert!(matches!(
        validate(&registry, disabled_with_provider),
        Err(DataExportSelectionError::DisabledWithProvider)
    ));
}

#[test]
fn production_rejects_bad_tls_credentials_and_provider_settings() {
    let registry = registry(Arc::new(FactoryCalls::default()));
    let mut no_tls = enabled("analytics", 1);
    no_tls.providers[0].transport_security.tls = false;
    assert!(matches!(
        validate(&registry, no_tls),
        Err(DataExportSelectionError::InvalidTransportSecurity(
            DatabaseSecurityError::TlsRequired
        ))
    ));

    let mut no_credentials = enabled("analytics", 1);
    no_credentials.providers[0]
        .transport_security
        .credentials_file = None;
    assert!(matches!(
        validate(&registry, no_credentials),
        Err(DataExportSelectionError::InvalidTransportSecurity(
            DatabaseSecurityError::CredentialsRequired
        ))
    ));

    let mut wrong_schema = enabled("analytics", 1);
    wrong_schema.providers[0].configuration = wrong_schema.providers[0]
        .configuration
        .clone()
        .with_setting("schema", ConfigurationValue::Text("public".to_owned()));
    assert!(matches!(
        validate(&registry, wrong_schema),
        Err(DataExportSelectionError::InvalidConfiguration { source, .. })
            if source.field_name() == Some("schema")
    ));

    let mut raw_sql = enabled("analytics", 1);
    raw_sql.providers[0].configuration = raw_sql.providers[0]
        .configuration
        .clone()
        .with_setting("sql", ConfigurationValue::Text("select 1".to_owned()));
    assert!(matches!(
        validate(&registry, raw_sql),
        Err(DataExportSelectionError::InvalidConfiguration { source, .. })
            if source.code() == ConfigurationErrorCode::UnknownField
    ));
}

#[test]
fn pending_destination_change_requires_matching_audited_discard() {
    let registry = registry(Arc::new(FactoryCalls::default()));
    let previous = ExportDestination {
        destination_id: destination("old-analytics"),
        configuration_revision: 7,
    };
    let backlog = ExportBacklogState {
        destination: Some(previous.clone()),
        pending_batches: 2,
        pending_records: 50,
    };

    let blocked = registry.validate(
        Environment::Production,
        enabled("new-analytics", 1),
        &backlog,
        DestinationTransition::Preserve,
    );
    assert!(matches!(
        blocked,
        Err(DataExportSelectionError::PendingDestinationChange)
    ));

    let accepted = registry
        .validate(
            Environment::Production,
            enabled("new-analytics", 1),
            &backlog,
            DestinationTransition::AuditedDiscard {
                audit_event_id: "audit-export-discard-42".to_owned(),
                destination: previous,
            },
        )
        .expect("matching audit permits explicit discard");
    let ValidatedDataExport::Enabled(selection) = accepted else {
        panic!("expected enabled provider");
    };
    assert_eq!(
        selection.discard_audit.as_deref(),
        Some("audit-export-discard-42")
    );
}

#[test]
fn disabling_with_pending_data_retains_the_discard_audit_proof() {
    let registry = registry(Arc::new(FactoryCalls::default()));
    let previous = ExportDestination {
        destination_id: destination("analytics"),
        configuration_revision: 7,
    };
    let selection = registry
        .validate(
            Environment::Production,
            DataExportConfiguration::disabled(),
            &ExportBacklogState {
                destination: Some(previous.clone()),
                pending_batches: 1,
                pending_records: 10,
            },
            DestinationTransition::AuditedDiscard {
                audit_event_id: "audit-disable-export-42".to_owned(),
                destination: previous,
            },
        )
        .expect("audited discard permits disabling");

    assert!(matches!(
        selection,
        ValidatedDataExport::Disabled { discard_audit }
            if discard_audit.as_deref() == Some("audit-disable-export-42")
    ));
}

#[test]
fn batches_remain_bound_to_the_validated_instance_and_revision() {
    let registry = registry(Arc::new(FactoryCalls::default()));
    let selection = validate(&registry, enabled("analytics", 7)).expect("valid selection");
    let batch: ExportBatch = serde_json::from_str(include_str!(
        "../../../crates/contracts/tests/fixtures/export-batch-v1.json"
    ))
    .expect("canonical export batch fixture");

    selection
        .validate_batch_destination(&batch)
        .expect("matching destination and revision");
    let changed = validate(&registry, enabled("analytics", 8)).expect("new revision validates");
    assert_eq!(
        changed.validate_batch_destination(&batch),
        Err(DataExportSelectionError::BatchDestinationMismatch)
    );
}
