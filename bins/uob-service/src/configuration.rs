use std::{collections::BTreeMap, error::Error, fmt, fs, net::SocketAddr, path::Path};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uob_application::{ConfigurationValue, CredentialReference, TargetConfiguration};
use uob_contracts::{ArtifactDigest, BridgeId, Environment, ReleaseId, TargetInstanceId};
use uob_ems_scada_http_target_adapter::EMS_SCADA_HTTP_TARGET_KIND;
use uob_external_export_adapter::{
    DataExportConfiguration, DatabaseProviderRegistry, DestinationTransition, ExportBacklogState,
    postgresql_configuration_schema,
};
use uob_mqtt_target_adapter::MQTT_TARGET_KIND;
use uob_target_adapter::{
    BridgeTargetSelection, ConfiguredTarget, NetworkEndpoint, TransportEncryption,
    TransportSecurity,
};

use crate::{ServiceComposition, StartupIdentityConfiguration, compose_with_data_export};
pub(crate) use registry::target_registry;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfiguration {
    bridge: BridgeConfiguration,
    #[serde(default)]
    diagnostics: crate::diagnostics::Configuration,
    #[serde(default)]
    release_read: crate::release_read::Configuration,
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
    #[serde(default)]
    charging: charging::Configuration,
    #[serde(default)]
    configuration_api: crate::configuration_api::Configuration,
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

#[derive(Clone, Deserialize, PartialEq, Serialize)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<TransportConfiguration>,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
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

#[derive(Clone, Deserialize, PartialEq, Serialize)]
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
    pub diagnostics: crate::diagnostics::Validated,
    pub release_read: crate::release_read::Validated,
    pub service: ServiceComposition<Value, Value>,
    pub management_address: SocketAddr,
    pub events: ValidatedEventClientConfiguration,
    pub deployment: Option<crate::deployment::DeploymentLayout>,
    pub charging: Option<charging::ValidatedChargingConfiguration>,
    pub shutdown_timeout: std::time::Duration,
    pub configuration_api: crate::configuration_api::Validated,
}

#[derive(Clone)]
pub(crate) struct ValidatedEventClientConfiguration {
    pub endpoint: Url,
    pub credentials_file: Option<String>,
}

pub(crate) fn load(path: &Path) -> Result<ValidatedServiceConfiguration, ConfigurationLoadError> {
    let mut configuration = read_document(path)?;
    staged::overlay(&mut configuration)?;
    let mut validated = validate(configuration)?;
    validated.configuration_api = validated.configuration_api.bound_to(path);
    Ok(validated)
}

fn read_document(path: &Path) -> Result<FileConfiguration, ConfigurationLoadError> {
    let document = fs::read_to_string(path).map_err(|_| ConfigurationLoadError::Unavailable)?;
    toml::from_str(&document).map_err(|_| ConfigurationLoadError::InvalidDocument)
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
    let charging = configuration.charging.validate(
        configuration.bridge.environment,
        configuration.management.listen_addr,
        &configuration.bridge.id,
    )?;

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

    let targets = target_registry(&bridge_id, configuration.bridge.environment)?;
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
    let diagnostics = configuration
        .diagnostics
        .validate(
            &service.application.identity().bridge_id,
            service.application.runtime_identity().environment,
        )
        .map_err(|_| ConfigurationLoadError::InvalidDocument)?;
    let release_read = configuration
        .release_read
        .validate(configuration.bridge.environment)
        .map_err(|_| ConfigurationLoadError::InvalidDocument)?;
    let events = validate_event_client(configuration.events, configuration.management.listen_addr)?;
    let configuration_api = configuration
        .configuration_api
        .validate(&bridge_id, configuration.bridge.environment)
        .map_err(|_| ConfigurationLoadError::InvalidConfigurationApi)?;

    Ok(ValidatedServiceConfiguration {
        diagnostics,
        release_read,
        service,
        management_address: configuration.management.listen_addr,
        charging,
        shutdown_timeout,
        deployment,
        events,
        configuration_api,
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
    UnavailableSecret,
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
    InvalidCharging,
    InvalidConfigurationApi,
    InvalidStagedTargets,
    StagedTargetsConflict,
    Composition,
}

impl ConfigurationLoadError {
    /// Stable sanitized code shared by the CLI and the configuration API.
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "configuration.unavailable",
            Self::UnavailableSecret => "configuration.unavailable_secret",
            Self::InvalidDocument => "configuration.invalid_document",
            Self::InvalidIdentity => "configuration.invalid_identity",
            Self::MissingTargetSelection => "configuration.missing_target_selection",
            Self::InvalidTargetSetting => "configuration.invalid_target_setting",
            Self::InvalidTransport => "configuration.invalid_transport",
            Self::UnsafeManagementListener => "configuration.unsafe_management_listener",
            Self::InvalidEventEndpoint => "configuration.invalid_event_endpoint",
            Self::UnsafeRemoteEventEndpoint => "configuration.unsafe_remote_event_endpoint",
            Self::UnavailableDataExport => "configuration.unavailable_data_export",
            Self::InvalidShutdownTimeout => "configuration.invalid_shutdown_timeout",
            Self::InvalidDeployment => "configuration.invalid_deployment",
            Self::UnsafeStagingNetwork => "configuration.unsafe_staging_network",
            Self::InvalidCharging => "configuration.invalid_charging",
            Self::InvalidConfigurationApi => "configuration.invalid_configuration_api",
            Self::InvalidStagedTargets => "configuration.invalid_staged_targets",
            Self::StagedTargetsConflict => "configuration.staged_targets_conflict",
            Self::Composition => "configuration.composition",
        }
    }
}

impl fmt::Display for ConfigurationLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "configuration {self:?}")
    }
}

impl Error for ConfigurationLoadError {}

#[cfg(test)]
mod tests;

pub(crate) mod charging;
mod registry;
mod secrets;
mod staged;
mod staging;
mod target_issues;
mod target_views;
pub(crate) mod targets;

pub(crate) fn check_secrets(path: &Path) -> Result<(), ConfigurationLoadError> {
    secrets::check(path)
}
