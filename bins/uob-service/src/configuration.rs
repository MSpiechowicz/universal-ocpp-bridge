use std::{collections::BTreeMap, error::Error, fmt, fs, net::SocketAddr, path::Path};

use reqwest::Url;
use serde::Deserialize;
use uob_application::{
    ConfigurationValue, CredentialReference, TargetCapability, TargetConfiguration,
};
use uob_contracts::{ArtifactDigest, BridgeId, Environment, ReleaseId, TargetInstanceId};
use uob_ems_scada_http_target_adapter::{EMS_SCADA_HTTP_TARGET_KIND, EmsScadaHttpTargetFactory};
use uob_external_export_adapter::{
    DataExportConfiguration, DatabaseProviderRegistry, DestinationTransition, ExportBacklogState,
    postgresql_configuration_schema,
};
use uob_mqtt_target_adapter::{
    EMS_SCADA_PROFILE, MQTT_TARGET_KIND, MqttTargetFactory, STANDARD_PROFILE,
};
use uob_target_adapter::{
    BridgeTargetSelection, ConfiguredTarget, NetworkEndpoint, TargetDisplayFamily, TargetPreset,
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
    #[serde(default)]
    lifecycle: crate::lifecycle::LifecycleConfiguration,
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
    pub deployment: Option<crate::deployment::DeploymentLayout>,
    pub shutdown_timeout: std::time::Duration,
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
    staging::validate(&configuration)?;
    let deployment =
        crate::deployment::DeploymentLayout::from_environment(configuration.bridge.environment)
            .map_err(|_| ConfigurationLoadError::InvalidDeployment)?;
    let shutdown_timeout = configuration
        .lifecycle
        .validate()
        .ok_or(ConfigurationLoadError::InvalidShutdownTimeout)?;
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
        .register(
            EmsScadaHttpTargetFactory::new(configuration.bridge.environment),
            ems_scada_http_registration(),
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
        shutdown_timeout,
        deployment,
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
    } else if entry.kind == EMS_SCADA_HTTP_TARGET_KIND {
        // The listener owns its own exposure rule: loopback needs no TLS, while a public
        // address requires explicit enablement, a TLS identity, and scoped credentials. The
        // shared outbound transport block would demand TLS even on loopback, so it is not
        // accepted for this kind.
        if entry.transport.is_some() {
            return Err(ConfigurationLoadError::InvalidTransport);
        }
        None
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
        presets: vec![
            TargetPreset {
                id: STANDARD_PROFILE.to_owned(),
                display_name: "Standard bridge namespace".to_owned(),
            },
            TargetPreset {
                id: EMS_SCADA_PROFILE.to_owned(),
                display_name: "EMS/SCADA over MQTT".to_owned(),
            },
        ],
        capabilities: vec![
            TargetCapability("retained-state".to_owned()),
            TargetCapability("redacted-tracing".to_owned()),
        ],
        transport_policy: Some(TransportPolicy::Outbound),
    }
}

fn ems_scada_http_registration() -> TargetRegistration {
    TargetRegistration {
        display_family: TargetDisplayFamily {
            id: "ems-scada".to_owned(),
            display_name: "EMS/SCADA".to_owned(),
        },
        presets: vec![],
        capabilities: vec![],
        // The factory validates listener exposure itself; see `configured_target`.
        transport_policy: None,
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
    InvalidShutdownTimeout,
    InvalidDeployment,
    UnsafeStagingNetwork,
    Composition,
}

impl fmt::Display for ConfigurationLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "configuration {self:?}")
    }
}

impl Error for ConfigurationLoadError {}

#[cfg(test)]
mod tests;

mod staging;
