use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use uob_application::{
    BridgeTarget, BridgeTargetFactory, ConfigurationError, ConfigurationErrorCode,
    ConfigurationField, ConfigurationFieldKind, ConfigurationSchema, ConfigurationValue,
    CredentialReference, TargetCapability, TargetConfiguration, ValidatedTargetConfiguration,
};
use uob_contracts::{BridgeId, Environment, TargetInstanceId};
use uob_target_adapter::{
    BridgeTargetSelection, ConfiguredTarget, EMS_SCADA_OPCUA_KIND, NetworkEndpoint,
    TargetDisplayFamily, TargetPreset, TargetRegistration, TargetRegistry, TargetSelectionError,
    TransportEncryption, TransportPolicy, TransportPolicyError, TransportSecurity,
};

#[derive(Default)]
struct FactoryCalls {
    validated: AtomicUsize,
    created: AtomicUsize,
}

struct FixtureFactory {
    kind: &'static str,
    schema: ConfigurationSchema,
    calls: Arc<FactoryCalls>,
    reject: bool,
}

impl BridgeTargetFactory<(), ()> for FixtureFactory {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn configuration_schema(&self) -> ConfigurationSchema {
        self.schema.clone()
    }

    fn validate(
        &self,
        configuration: &TargetConfiguration,
    ) -> Result<ValidatedTargetConfiguration, ConfigurationError> {
        self.calls.validated.fetch_add(1, Ordering::SeqCst);
        if self.reject {
            return Err(ConfigurationError::new(ConfigurationErrorCode::Unsupported));
        }
        if let Some(ConfigurationValue::Text(profile)) = configuration.setting("profile")
            && profile != "ems-scada"
        {
            return Err(ConfigurationError::field(
                ConfigurationErrorCode::InvalidField,
                "profile",
            ));
        }
        Ok(ValidatedTargetConfiguration::new(configuration.clone()))
    }

    fn create(
        &self,
        _configuration: ValidatedTargetConfiguration,
    ) -> Result<Box<dyn BridgeTarget<(), ()>>, ConfigurationError> {
        self.calls.created.fetch_add(1, Ordering::SeqCst);
        Err(ConfigurationError::new(ConfigurationErrorCode::Unsupported))
    }
}

fn text<T, E: std::fmt::Debug>(constructor: impl FnOnce(String) -> Result<T, E>, value: &str) -> T {
    constructor(value.to_owned()).expect("valid fixture identity")
}

fn field(name: &str, kind: ConfigurationFieldKind, required: bool) -> ConfigurationField {
    ConfigurationField {
        name: name.to_owned(),
        kind,
        required,
    }
}

fn mqtt_schema() -> ConfigurationSchema {
    ConfigurationSchema {
        fields: vec![
            field("broker_url", ConfigurationFieldKind::Text, true),
            field(
                "credentials_file",
                ConfigurationFieldKind::CredentialReference,
                true,
            ),
            field("profile", ConfigurationFieldKind::Text, false),
        ],
    }
}

fn http_schema() -> ConfigurationSchema {
    ConfigurationSchema {
        fields: vec![
            field("listen_addr", ConfigurationFieldKind::Text, true),
            field(
                "credentials_file",
                ConfigurationFieldKind::CredentialReference,
                true,
            ),
        ],
    }
}

fn registration(
    family: &str,
    transport_policy: TransportPolicy,
    presets: Vec<TargetPreset>,
) -> TargetRegistration {
    TargetRegistration {
        display_family: TargetDisplayFamily {
            id: family.to_owned(),
            display_name: family.to_owned(),
        },
        presets,
        capabilities: vec![TargetCapability("canonical-state".to_owned())],
        transport_policy: Some(transport_policy),
    }
}

fn registry(
    mqtt_calls: Arc<FactoryCalls>,
    http_calls: Arc<FactoryCalls>,
    reject_http: bool,
) -> TargetRegistry<(), ()> {
    let mut registry = TargetRegistry::new();
    registry
        .register(
            FixtureFactory {
                kind: "mqtt",
                schema: mqtt_schema(),
                calls: mqtt_calls,
                reject: false,
            },
            registration(
                "mqtt",
                TransportPolicy::Outbound,
                vec![TargetPreset {
                    id: "ems-scada".to_owned(),
                    display_name: "EMS/SCADA over MQTT".to_owned(),
                }],
            ),
        )
        .expect("register mqtt");
    registry
        .register(
            FixtureFactory {
                kind: "ems-scada.http",
                schema: http_schema(),
                calls: http_calls,
                reject: reject_http,
            },
            registration("ems-scada", TransportPolicy::Listener, vec![]),
        )
        .expect("register HTTP target");
    registry
}

fn credential() -> CredentialReference {
    CredentialReference::new("/etc/uob/secrets/target.toml").expect("credential reference")
}

fn tls(endpoint: &str) -> TransportSecurity {
    TransportSecurity {
        endpoint: NetworkEndpoint::parse(endpoint).expect("valid endpoint"),
        encryption: TransportEncryption::Tls,
        certificate_verification: true,
        credentials: Some(credential()),
        explicitly_isolated: false,
    }
}

fn plaintext_demo(endpoint: &str) -> TransportSecurity {
    TransportSecurity {
        endpoint: NetworkEndpoint::parse(endpoint).expect("valid endpoint"),
        encryption: TransportEncryption::Plaintext,
        certificate_verification: false,
        credentials: None,
        explicitly_isolated: true,
    }
}

fn configured_target(
    id: &str,
    kind: &str,
    enabled: bool,
    settings: &[(&str, ConfigurationValue)],
    transport_security: Option<TransportSecurity>,
) -> ConfiguredTarget {
    let mut configuration = TargetConfiguration::new(text(TargetInstanceId::new, id), 1);
    for (name, value) in settings {
        configuration = configuration.with_setting(*name, value.clone());
    }
    ConfiguredTarget {
        kind: kind.to_owned(),
        enabled,
        configuration,
        transport_security,
    }
}

fn selection(
    environment: Environment,
    target_id: &str,
    targets: Vec<ConfiguredTarget>,
) -> BridgeTargetSelection {
    BridgeTargetSelection {
        bridge_id: text(BridgeId::new, "bridge-1"),
        environment,
        target_id: text(TargetInstanceId::new, target_id),
        targets,
    }
}

fn mqtt_settings(profile: Option<&str>) -> Vec<(&'static str, ConfigurationValue)> {
    let mut settings = vec![
        (
            "broker_url",
            ConfigurationValue::Text("mqtts://broker.example:8883".to_owned()),
        ),
        (
            "credentials_file",
            ConfigurationValue::CredentialReference(credential()),
        ),
    ];
    if let Some(profile) = profile {
        settings.push(("profile", ConfigurationValue::Text(profile.to_owned())));
    }
    settings
}

fn http_settings() -> Vec<(&'static str, ConfigurationValue)> {
    vec![
        (
            "listen_addr",
            ConfigurationValue::Text("127.0.0.1:9080".to_owned()),
        ),
        (
            "credentials_file",
            ConfigurationValue::CredentialReference(credential()),
        ),
    ]
}

#[test]
fn catalog_and_validation_share_factory_schema_without_credentials() {
    let mqtt_calls = Arc::new(FactoryCalls::default());
    let registry = registry(
        Arc::clone(&mqtt_calls),
        Arc::new(FactoryCalls::default()),
        false,
    );

    let catalog = registry.catalog();
    let mqtt = catalog
        .iter()
        .find(|entry| entry.kind.as_str() == "mqtt")
        .expect("mqtt catalog entry");
    assert_eq!(mqtt.display_family.id, "mqtt");
    assert_eq!(mqtt.configuration_schema, mqtt_schema());
    assert_eq!(mqtt.presets[0].id, "ems-scada");
    assert!(!format!("{catalog:?}").contains("/etc/uob/secrets"));

    let settings = mqtt_settings(None);
    let validated = registry
        .validate(selection(
            Environment::Production,
            "main",
            vec![configured_target(
                "main",
                "mqtt",
                true,
                &settings,
                Some(tls("mqtts://broker.example:8883")),
            )],
        ))
        .expect("valid MQTT selection");
    assert_eq!(validated.catalog.configuration_schema, mqtt_schema());
    assert_eq!(mqtt_calls.validated.load(Ordering::SeqCst), 1);
    assert_eq!(mqtt_calls.created.load(Ordering::SeqCst), 0);
}

#[test]
fn direct_http_and_mqtt_ems_preset_are_distinct_valid_selections() {
    let mqtt_calls = Arc::new(FactoryCalls::default());
    let http_calls = Arc::new(FactoryCalls::default());
    let registry = registry(Arc::clone(&mqtt_calls), Arc::clone(&http_calls), false);

    let mqtt = mqtt_settings(Some("ems-scada"));
    registry
        .validate(selection(
            Environment::Production,
            "broker-path",
            vec![configured_target(
                "broker-path",
                "mqtt",
                true,
                &mqtt,
                Some(tls("mqtts://broker.example:8883")),
            )],
        ))
        .expect("EMS MQTT preset uses the MQTT factory");

    let http = http_settings();
    registry
        .validate(selection(
            Environment::Demo,
            "direct-api",
            vec![configured_target(
                "direct-api",
                "ems-scada.http",
                true,
                &http,
                Some(plaintext_demo("127.0.0.1:9080")),
            )],
        ))
        .expect("loopback demo HTTP listener");

    assert_eq!(mqtt_calls.validated.load(Ordering::SeqCst), 1);
    assert_eq!(http_calls.validated.load(Ordering::SeqCst), 1);
    assert_eq!(registry.len(), 2);
}

#[test]
fn disabled_target_is_neither_validated_nor_constructed() {
    let mqtt_calls = Arc::new(FactoryCalls::default());
    let http_calls = Arc::new(FactoryCalls::default());
    let registry = registry(Arc::clone(&mqtt_calls), Arc::clone(&http_calls), false);
    let mqtt = mqtt_settings(None);

    registry
        .validate(selection(
            Environment::Production,
            "main",
            vec![
                configured_target(
                    "main",
                    "mqtt",
                    true,
                    &mqtt,
                    Some(tls("mqtts://broker.example:8883")),
                ),
                configured_target("standby", "ems-scada.http", false, &[], None),
            ],
        ))
        .expect("disabled settings are not activated");

    assert_eq!(mqtt_calls.validated.load(Ordering::SeqCst), 1);
    assert_eq!(http_calls.validated.load(Ordering::SeqCst), 0);
    assert_eq!(mqtt_calls.created.load(Ordering::SeqCst), 0);
    assert_eq!(http_calls.created.load(Ordering::SeqCst), 0);
}

#[test]
fn selection_rejects_unknown_unavailable_missing_and_duplicate_targets() {
    let mut registry = registry(
        Arc::new(FactoryCalls::default()),
        Arc::new(FactoryCalls::default()),
        false,
    );
    registry
        .declare_first_release_unavailable_targets()
        .expect("declare unavailable first-release targets");

    let opcua = registry
        .catalog()
        .into_iter()
        .find(|entry| entry.kind.as_str() == EMS_SCADA_OPCUA_KIND)
        .expect("reserved OPC UA catalog entry");
    assert_eq!(opcua.display_family.id, "ems-scada");
    assert!(!opcua.available);
    assert!(!registry.contains(EMS_SCADA_OPCUA_KIND));

    let unknown = configured_target("main", "unknown", true, &[], None);
    assert!(matches!(
        registry.validate(selection(Environment::Demo, "main", vec![unknown])),
        Err(TargetSelectionError::UnknownKind(kind)) if kind == "unknown"
    ));
    let unavailable = configured_target("main", EMS_SCADA_OPCUA_KIND, true, &[], None);
    assert!(matches!(
        registry.validate(selection(Environment::Demo, "main", vec![unavailable])),
        Err(TargetSelectionError::UnavailableKind(kind)) if kind == EMS_SCADA_OPCUA_KIND
    ));
    assert!(matches!(
        registry.validate(selection(Environment::Demo, "main", vec![])),
        Err(TargetSelectionError::MissingActiveTarget)
    ));

    let mqtt = mqtt_settings(None);
    let http = http_settings();
    let two_active = vec![
        configured_target(
            "main",
            "mqtt",
            true,
            &mqtt,
            Some(tls("mqtts://broker.example:8883")),
        ),
        configured_target(
            "second",
            "ems-scada.http",
            true,
            &http,
            Some(tls("https://0.0.0.0:9080")),
        ),
    ];
    assert!(matches!(
        registry.validate(selection(Environment::Production, "main", two_active)),
        Err(TargetSelectionError::MultipleActiveTargets)
    ));
}

#[test]
fn invalid_settings_and_production_transports_are_rejected_offline() {
    let registry = registry(
        Arc::new(FactoryCalls::default()),
        Arc::new(FactoryCalls::default()),
        false,
    );
    let missing_credentials = vec![(
        "broker_url",
        ConfigurationValue::Text("mqtts://broker.example:8883".to_owned()),
    )];
    assert!(matches!(
        registry.validate(selection(
            Environment::Production,
            "main",
            vec![configured_target(
                "main",
                "mqtt",
                true,
                &missing_credentials,
                Some(tls("mqtts://broker.example:8883")),
            )],
        )),
        Err(TargetSelectionError::InvalidConfiguration { source, .. })
            if source.code() == ConfigurationErrorCode::MissingField
    ));

    let settings = http_settings();
    let insecure = TransportSecurity {
        endpoint: NetworkEndpoint::parse("0.0.0.0:9080").expect("endpoint"),
        encryption: TransportEncryption::Plaintext,
        certificate_verification: false,
        credentials: None,
        explicitly_isolated: false,
    };
    assert!(matches!(
        registry.validate(selection(
            Environment::Production,
            "main",
            vec![configured_target(
                "main",
                "ems-scada.http",
                true,
                &settings,
                Some(insecure),
            )],
        )),
        Err(TargetSelectionError::InvalidTransportSecurity(
            TransportPolicyError::PlaintextRequiresIsolatedDemo
        ))
    ));
}

#[test]
fn failed_http_validation_never_falls_back_to_mqtt() {
    let mqtt_calls = Arc::new(FactoryCalls::default());
    let http_calls = Arc::new(FactoryCalls::default());
    let registry = registry(Arc::clone(&mqtt_calls), Arc::clone(&http_calls), true);
    let settings = http_settings();

    assert!(matches!(
        registry.validate(selection(
            Environment::Demo,
            "main",
            vec![configured_target(
                "main",
                "ems-scada.http",
                true,
                &settings,
                Some(plaintext_demo("127.0.0.1:9080")),
            )],
        )),
        Err(TargetSelectionError::InvalidConfiguration { kind, .. })
            if kind == "ems-scada.http"
    ));
    assert_eq!(http_calls.validated.load(Ordering::SeqCst), 1);
    assert_eq!(mqtt_calls.validated.load(Ordering::SeqCst), 0);
}
