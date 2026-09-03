use std::{collections::BTreeMap, error::Error, fmt, fs, net::SocketAddr, path::Path};

use reqwest::Url;
use serde::Deserialize;
use uob_application::{
    ConfigurationValue, CredentialReference, TargetCapability, TargetConfiguration,
};
use uob_contracts::{ArtifactDigest, BridgeId, Environment, ReleaseId, TargetInstanceId};
use uob_external_export_adapter::{
    DataExportConfiguration, DatabaseProviderRegistry, DestinationTransition, ExportBacklogState,
    postgresql_configuration_schema,
};
use uob_mqtt_target_adapter::{MQTT_TARGET_KIND, MqttTargetFactory};
use uob_target_adapter::{
    BridgeTargetSelection, ConfiguredTarget, NetworkEndpoint, TargetDisplayFamily,
    TargetRegistration, TargetRegistry, TransportEncryption, TransportPolicy, TransportSecurity,
};

use crate::{ServiceComposition, StartupIdentityConfiguration, compose_with_data_export};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfiguration {
    bridge: BridgeConfiguration,
    #[serde(default)]
    management: ManagementConfiguration,
    #[serde(default)]
    events: EventClientConfiguration,
    #[serde(default)]
    targets: Vec<TargetEntry>,
    #[serde(default)]
    data_export: DataExportSection,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BridgeConfiguration {
    id: String,
    #[serde(default = "production")]
    environment: Environment,
    target_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ManagementConfiguration {
    listen_addr: SocketAddr,
}

impl Default for ManagementConfiguration {
    fn default() -> Self {
        Self {
            listen_addr: uob_management_adapter::DEFAULT_MANAGEMENT_LISTEN_ADDRESS,
        }
    }
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct EventClientConfiguration {
    pub endpoint: Option<String>,
    pub credentials_file: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetEntry {
    id: String,
    kind: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default = "initial_revision")]
    revision: u64,
    #[serde(default)]
    settings: BTreeMap<String, toml::Value>,
    transport: Option<TransportConfiguration>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransportConfiguration {
    endpoint: String,
    encryption: TransportEncryptionConfiguration,
    #[serde(default)]
    certificate_verification: bool,
    credentials_file: Option<String>,
    #[serde(default)]
    explicitly_isolated: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum TransportEncryptionConfiguration {
    Tls,
    Plaintext,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DataExportSection {
    enabled: bool,
}

pub(crate) struct ValidatedServiceConfiguration {
    pub service: ServiceComposition<(), ()>,
    pub management_address: SocketAddr,
    pub events: ValidatedEventClientConfiguration,
}

#[derive(Clone)]
pub(crate) struct ValidatedEventClientConfiguration {
    pub endpoint: Url,
    pub credentials_file: Option<String>,
}

pub(crate) fn load(path: &Path) -> Result<ValidatedServiceConfiguration, ConfigurationLoadError> {
    let document = fs::read_to_string(path).map_err(|_| ConfigurationLoadError::Unavailable)?;
    let configuration: FileConfiguration =
        toml::from_str(&document).map_err(|_| ConfigurationLoadError::InvalidDocument)?;
    validate(configuration)
}

fn validate(
    configuration: FileConfiguration,
) -> Result<ValidatedServiceConfiguration, ConfigurationLoadError> {
    if !configuration.management.listen_addr.ip().is_loopback() {
        return Err(ConfigurationLoadError::UnsafeManagementListener);
    }
    if configuration.data_export.enabled {
        return Err(ConfigurationLoadError::UnavailableDataExport);
    }

    let bridge_id = BridgeId::new(configuration.bridge.id)
        .map_err(|_| ConfigurationLoadError::InvalidIdentity)?;
    let target_selection = target_selection(
        &bridge_id,
        configuration.bridge.environment,
        configuration.bridge.target_id,
        configuration.targets,
    )?;
    let identity = StartupIdentityConfiguration::production(
        bridge_id.clone(),
        ReleaseId::new(env!("CARGO_PKG_VERSION"))
            .map_err(|_| ConfigurationLoadError::InvalidIdentity)?,
        ArtifactDigest::new(option_env!("UOB_RELEASE_DIGEST").unwrap_or("sha256:development"))
            .map_err(|_| ConfigurationLoadError::InvalidIdentity)?,
    )
    .in_environment(configuration.bridge.environment);

    let mut targets = TargetRegistry::<(), ()>::new();
    targets
        .register(
            MqttTargetFactory::new(&bridge_id, configuration.bridge.environment)
                .map_err(|_| ConfigurationLoadError::Composition)?,
            mqtt_registration(),
        )
        .map_err(|_| ConfigurationLoadError::Composition)?;
    targets
        .declare_first_release_unavailable_targets()
        .map_err(|_| ConfigurationLoadError::Composition)?;
    let mut providers = DatabaseProviderRegistry::new();
    providers
        .declare_postgresql_unavailable(postgresql_configuration_schema())
        .map_err(|_| ConfigurationLoadError::Composition)?;
    let service = compose_with_data_export(
        targets,
        providers,
        identity,
        target_selection,
        DataExportConfiguration::disabled(),
        &ExportBacklogState::default(),
        DestinationTransition::Preserve,
    )
    .map_err(|_| ConfigurationLoadError::Composition)?;
    let events = validate_event_client(configuration.events, configuration.management.listen_addr)?;

    Ok(ValidatedServiceConfiguration {
        service,
        management_address: configuration.management.listen_addr,
        events,
    })
}

fn target_selection(
    bridge_id: &BridgeId,
    environment: Environment,
    target_id: Option<String>,
    targets: Vec<TargetEntry>,
) -> Result<Option<BridgeTargetSelection>, ConfigurationLoadError> {
    if target_id.is_none() && targets.is_empty() {
        return Ok(None);
    }
    let target_id =
        TargetInstanceId::new(target_id.ok_or(ConfigurationLoadError::MissingTargetSelection)?)
            .map_err(|_| ConfigurationLoadError::InvalidIdentity)?;
    let targets = targets
        .into_iter()
        .map(|entry| configured_target(entry, environment))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(BridgeTargetSelection {
        bridge_id: bridge_id.clone(),
        environment,
        target_id,
        targets,
    }))
}

fn configured_target(
    entry: TargetEntry,
    environment: Environment,
) -> Result<ConfiguredTarget, ConfigurationLoadError> {
    let instance_id =
        TargetInstanceId::new(entry.id).map_err(|_| ConfigurationLoadError::InvalidIdentity)?;
    let mut configuration = TargetConfiguration::new(instance_id, entry.revision);
    for (name, value) in entry.settings {
        configuration = configuration.with_setting(name.clone(), setting(&name, value)?);
    }
    let transport_security = if entry.kind == MQTT_TARGET_KIND {
        if entry.transport.is_some() {
            return Err(ConfigurationLoadError::InvalidTransport);
        }
        Some(mqtt_transport(environment, &configuration)?)
    } else {
        entry.transport.map(transport).transpose()?
    };
    Ok(ConfiguredTarget {
        kind: entry.kind,
        enabled: entry.enabled,
        configuration,
        transport_security,
    })
}

fn mqtt_transport(
    environment: Environment,
    configuration: &TargetConfiguration,
) -> Result<TransportSecurity, ConfigurationLoadError> {
    let Some(ConfigurationValue::Text(broker_url)) = configuration.setting("broker_url") else {
        return Err(ConfigurationLoadError::InvalidTransport);
    };
    let (encryption, certificate_verification) = if broker_url.starts_with("mqtts://") {
        (TransportEncryption::Tls, true)
    } else if broker_url.starts_with("mqtt://") {
        (TransportEncryption::Plaintext, false)
    } else {
        return Err(ConfigurationLoadError::InvalidTransport);
    };
    let allow_plaintext = match configuration.setting("allow_plaintext") {
        Some(ConfigurationValue::Boolean(value)) => *value,
        None => false,
        _ => return Err(ConfigurationLoadError::InvalidTransport),
    };
    match (encryption, environment, allow_plaintext) {
        (TransportEncryption::Tls, _, false)
        | (TransportEncryption::Plaintext, Environment::Demo, true) => {}
        _ => return Err(ConfigurationLoadError::InvalidTransport),
    }
    let credentials = match configuration.setting("credentials_file") {
        Some(ConfigurationValue::CredentialReference(reference)) => Some(reference.clone()),
        None => None,
        _ => return Err(ConfigurationLoadError::InvalidTransport),
    };
    Ok(TransportSecurity {
        endpoint: NetworkEndpoint::parse(broker_url)
            .map_err(|_| ConfigurationLoadError::InvalidTransport)?,
        encryption,
        certificate_verification,
        credentials,
        access_policy: None,
        explicitly_isolated: allow_plaintext,
    })
}

fn mqtt_registration() -> TargetRegistration {
    TargetRegistration {
        display_family: TargetDisplayFamily {
            id: "mqtt".to_owned(),
            display_name: "MQTT".to_owned(),
        },
        presets: vec![],
        capabilities: vec![
            TargetCapability("retained-state".to_owned()),
            TargetCapability("redacted-tracing".to_owned()),
        ],
        transport_policy: Some(TransportPolicy::Outbound),
    }
}

fn setting(name: &str, value: toml::Value) -> Result<ConfigurationValue, ConfigurationLoadError> {
    match value {
        toml::Value::String(value) if is_credential_field(name) => CredentialReference::new(value)
            .map(ConfigurationValue::CredentialReference)
            .map_err(|_| ConfigurationLoadError::InvalidTargetSetting),
        toml::Value::String(value) => Ok(ConfigurationValue::Text(value)),
        toml::Value::Integer(value) => Ok(ConfigurationValue::Integer(value)),
        toml::Value::Boolean(value) => Ok(ConfigurationValue::Boolean(value)),
        _ => Err(ConfigurationLoadError::InvalidTargetSetting),
    }
}

fn is_credential_field(name: &str) -> bool {
    name.ends_with("_file") || name.contains("credential")
}

fn transport(
    configuration: TransportConfiguration,
) -> Result<TransportSecurity, ConfigurationLoadError> {
    Ok(TransportSecurity {
        endpoint: NetworkEndpoint::parse(&configuration.endpoint)
            .map_err(|_| ConfigurationLoadError::InvalidTransport)?,
        encryption: match configuration.encryption {
            TransportEncryptionConfiguration::Tls => TransportEncryption::Tls,
            TransportEncryptionConfiguration::Plaintext => TransportEncryption::Plaintext,
        },
        certificate_verification: configuration.certificate_verification,
        credentials: configuration
            .credentials_file
            .map(CredentialReference::new)
            .transpose()
            .map_err(|_| ConfigurationLoadError::InvalidTransport)?,
        access_policy: None,
        explicitly_isolated: configuration.explicitly_isolated,
    })
}

fn validate_event_client(
    configuration: EventClientConfiguration,
    management_address: SocketAddr,
) -> Result<ValidatedEventClientConfiguration, ConfigurationLoadError> {
    let endpoint = configuration
        .endpoint
        .unwrap_or_else(|| format!("http://{management_address}/api/v1/events"));
    let endpoint =
        Url::parse(&endpoint).map_err(|_| ConfigurationLoadError::InvalidEventEndpoint)?;
    if endpoint.username() != "" || endpoint.password().is_some() || endpoint.query().is_some() {
        return Err(ConfigurationLoadError::InvalidEventEndpoint);
    }
    let local = endpoint.host_str().is_some_and(is_loopback_host);
    if !local && (endpoint.scheme() != "https" || configuration.credentials_file.is_none()) {
        return Err(ConfigurationLoadError::UnsafeRemoteEventEndpoint);
    }
    if !matches!(endpoint.scheme(), "http" | "https") {
        return Err(ConfigurationLoadError::InvalidEventEndpoint);
    }
    Ok(ValidatedEventClientConfiguration {
        endpoint,
        credentials_file: configuration.credentials_file,
    })
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

const fn production() -> Environment {
    Environment::Production
}

const fn initial_revision() -> u64 {
    1
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfigurationLoadError {
    Unavailable,
    InvalidDocument,
    InvalidIdentity,
    MissingTargetSelection,
    InvalidTargetSetting,
    InvalidTransport,
    UnsafeManagementListener,
    InvalidEventEndpoint,
    UnsafeRemoteEventEndpoint,
    UnavailableDataExport,
    Composition,
}

impl fmt::Display for ConfigurationLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "configuration {self:?}")
    }
}

impl Error for ConfigurationLoadError {}

#[cfg(test)]
mod tests {
    use super::{ConfigurationLoadError, FileConfiguration, validate};

    #[test]
    fn configuration_rejects_unknown_fields_and_embedded_event_credentials() {
        let unknown = "[bridge]\nid='demo'\nunknown=true\n";
        assert!(toml::from_str::<FileConfiguration>(unknown).is_err());

        let document = "[bridge]\nid='demo'\nenvironment='demo'\n[events]\nendpoint='http://user:secret@localhost:8080/api/v1/events'\n";
        let parsed = toml::from_str(document).expect("configuration syntax");
        assert!(matches!(
            validate(parsed),
            Err(ConfigurationLoadError::InvalidEventEndpoint)
        ));
    }

    #[test]
    fn api_only_configuration_uses_shared_composition() {
        let document =
            "[bridge]\nid='demo'\nenvironment='demo'\n[management]\nlisten_addr='127.0.0.1:0'\n";
        let parsed = toml::from_str(document).expect("configuration syntax");
        let validated = validate(parsed).expect("valid configuration");

        assert_eq!(
            validated.service.application.identity().bridge_id.as_str(),
            "demo"
        );
        assert!(validated.service.target_selection.is_none());
        assert_eq!(validated.management_address.port(), 0);
    }

    #[test]
    fn remote_events_require_https_and_a_credential_reference() {
        let document =
            "[bridge]\nid='demo'\n[events]\nendpoint='http://events.example/api/v1/events'\n";
        let parsed = toml::from_str(document).expect("configuration syntax");
        assert!(matches!(
            validate(parsed),
            Err(ConfigurationLoadError::UnsafeRemoteEventEndpoint)
        ));
    }

    #[test]
    fn mqtt_uses_one_broker_url_and_trusted_runtime_namespace() {
        let document = "[bridge]\nid='site-01'\nenvironment='demo'\ntarget_id='main'\n\n[[targets]]\nid='main'\nkind='mqtt'\nenabled=true\n\n[targets.settings]\nbroker_url='mqtt://127.0.0.1:1883'\nallow_plaintext=true\n";
        let parsed = toml::from_str(document).expect("configuration syntax");
        let validated = validate(parsed).expect("valid isolated MQTT configuration");

        assert!(validated.service.targets.contains("mqtt"));
        assert!(validated.service.target_selection.is_some());

        let duplicate = "[bridge]\nid='site-01'\nenvironment='demo'\ntarget_id='main'\n\n[[targets]]\nid='main'\nkind='mqtt'\nenabled=true\ntransport={ endpoint='mqtt://other:1883', encryption='plaintext', explicitly_isolated=true }\n\n[targets.settings]\nbroker_url='mqtt://127.0.0.1:1883'\nallow_plaintext=true\n";
        let parsed = toml::from_str(duplicate).expect("configuration syntax");
        assert!(matches!(
            validate(parsed),
            Err(ConfigurationLoadError::InvalidTransport)
        ));

        let implicit_plaintext = "[bridge]\nid='site-01'\nenvironment='demo'\ntarget_id='main'\n\n[[targets]]\nid='main'\nkind='mqtt'\nenabled=true\n\n[targets.settings]\nbroker_url='mqtt://127.0.0.1:1883'\n";
        let parsed = toml::from_str(implicit_plaintext).expect("configuration syntax");
        assert!(matches!(
            validate(parsed),
            Err(ConfigurationLoadError::InvalidTransport)
        ));
    }
}
