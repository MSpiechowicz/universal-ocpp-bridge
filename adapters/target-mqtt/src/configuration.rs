use std::time::Duration;

use uob_application::{
    BridgeTarget, BridgeTargetFactory, ConfigurationError, ConfigurationErrorCode,
    ConfigurationField, ConfigurationFieldKind, ConfigurationSchema, ConfigurationValue,
    CredentialReference, TargetConfiguration, ValidatedTargetConfiguration,
};
use uob_contracts::{BridgeId, Environment};
use url::Url;

use crate::{mapping::TopicNamespace, target::MqttTarget};

pub use self::profile::{EMS_SCADA_PROFILE, STANDARD_PROFILE};
pub(crate) use self::{
    credentials::{ResolvedCredentials, resolve_credentials},
    profile::MqttProfile,
};

mod credentials;
mod profile;
#[cfg(test)]
mod tests;

/// Stable registry kind for the MQTT target.
pub const MQTT_TARGET_KIND: &str = "mqtt";

/// Adapter-owned runtime bounds. They are fixed by the composition root, not broker input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MqttRuntimeOptions {
    /// Bounded rumqttc request channel capacity.
    pub request_capacity: usize,
    /// Largest canonical JSON payload accepted by the mapping.
    pub maximum_message_bytes: usize,
    /// Largest number of host deliveries awaiting broker acknowledgement.
    pub maximum_in_flight_deliveries: usize,
    /// Largest number of target-originated commands awaiting host admission.
    pub maximum_in_flight_commands: usize,
    /// Largest retained station-state cache used for reconnect republishing.
    pub retained_state_capacity: usize,
    /// Largest retained Home Assistant discovery cache used for reconnect republishing.
    pub discovery_capacity: usize,
    /// Largest retained EMS/SCADA point-catalog cache used for reconnect republishing.
    pub point_catalog_capacity: usize,
    /// MQTT keepalive interval.
    pub keep_alive: Duration,
    /// TCP/TLS connection timeout in seconds.
    pub connection_timeout_seconds: u64,
    /// Maximum time outstanding protocol work may make no observable progress.
    pub no_progress_timeout: Duration,
    /// Initial delay before polling for a reconnect.
    pub reconnect_initial_backoff: Duration,
    /// Maximum reconnect delay.
    pub reconnect_maximum_backoff: Duration,
}

impl Default for MqttRuntimeOptions {
    fn default() -> Self {
        Self {
            request_capacity: 32,
            maximum_message_bytes: 256 * 1024,
            maximum_in_flight_deliveries: 16,
            maximum_in_flight_commands: 8,
            retained_state_capacity: 64,
            discovery_capacity: 512,
            point_catalog_capacity: 512,
            keep_alive: Duration::from_secs(15),
            connection_timeout_seconds: 2,
            no_progress_timeout: Duration::from_secs(30),
            reconnect_initial_backoff: Duration::from_millis(100),
            reconnect_maximum_backoff: Duration::from_secs(5),
        }
    }
}

impl MqttRuntimeOptions {
    fn validate(self) -> Result<Self, ConfigurationError> {
        if self.request_capacity == 0
            || self.maximum_message_bytes == 0
            || self.maximum_in_flight_deliveries == 0
            || self.maximum_in_flight_deliveries > usize::from(u16::MAX)
            || self.maximum_in_flight_commands == 0
            || self.retained_state_capacity == 0
            || self.discovery_capacity == 0
            || self.point_catalog_capacity == 0
            || self.keep_alive < Duration::from_secs(1)
            || self.keep_alive.subsec_nanos() != 0
            || self.keep_alive.as_secs() > u64::from(u16::MAX)
            || self.connection_timeout_seconds == 0
            || self.no_progress_timeout.is_zero()
            || self.reconnect_initial_backoff.is_zero()
            || self.reconnect_maximum_backoff < self.reconnect_initial_backoff
        {
            return Err(ConfigurationError::new(
                ConfigurationErrorCode::InvalidField,
            ));
        }
        Ok(self)
    }
}

/// Network-free factory bound to the trusted process bridge and environment identity.
#[derive(Clone)]
pub struct MqttTargetFactory {
    environment: Environment,
    topics: TopicNamespace,
    runtime: MqttRuntimeOptions,
}

impl MqttTargetFactory {
    /// Creates a factory whose namespace cannot be overridden by target settings or payloads.
    ///
    /// # Errors
    ///
    /// Rejects a bridge identity whose encoded namespace would exceed MQTT string limits.
    pub fn new(bridge_id: &BridgeId, environment: Environment) -> Result<Self, ConfigurationError> {
        let topics = TopicNamespace::new(environment, bridge_id)?;
        Ok(Self {
            environment,
            topics,
            runtime: MqttRuntimeOptions::default(),
        })
    }

    /// Replaces fixed process-owned bounds, primarily for constrained deployments and tests.
    ///
    /// # Errors
    ///
    /// Rejects zero, inverted, or protocol-inexpressible bounds.
    pub fn with_runtime_options(
        mut self,
        runtime: MqttRuntimeOptions,
    ) -> Result<Self, ConfigurationError> {
        self.runtime = runtime.validate()?;
        Ok(self)
    }

    fn settings(
        &self,
        configuration: &TargetConfiguration,
    ) -> Result<MqttSettings, ConfigurationError> {
        mqtt_configuration_schema().validate_shape(configuration)?;
        let Some(ConfigurationValue::Text(broker_url)) = configuration.setting("broker_url") else {
            return Err(field(ConfigurationErrorCode::MissingField, "broker_url"));
        };
        let endpoint = BrokerEndpoint::parse(broker_url)?;
        let allow_plaintext = match configuration.setting("allow_plaintext") {
            Some(ConfigurationValue::Boolean(value)) => *value,
            None => false,
            _ => {
                return Err(field(
                    ConfigurationErrorCode::InvalidField,
                    "allow_plaintext",
                ));
            }
        };
        match (endpoint.tls, self.environment, allow_plaintext) {
            (true, _, false) | (false, Environment::Demo, true) => {}
            (true, _, true) => {
                return Err(field(
                    ConfigurationErrorCode::InvalidField,
                    "allow_plaintext",
                ));
            }
            (false, Environment::Demo, false) => {
                return Err(field(
                    ConfigurationErrorCode::MissingField,
                    "allow_plaintext",
                ));
            }
            (false, _, _) => {
                return Err(field(ConfigurationErrorCode::InvalidField, "broker_url"));
            }
        }
        let credentials_file = match configuration.setting("credentials_file") {
            Some(ConfigurationValue::CredentialReference(value)) => Some(value.clone()),
            None => None,
            _ => {
                return Err(field(
                    ConfigurationErrorCode::InvalidField,
                    "credentials_file",
                ));
            }
        };
        if self.environment == Environment::Production && credentials_file.is_none() {
            return Err(field(
                ConfigurationErrorCode::MissingField,
                "credentials_file",
            ));
        }
        let profile = match configuration.setting("profile") {
            Some(ConfigurationValue::Text(value)) => MqttProfile::parse(value)?,
            None => MqttProfile::default(),
            _ => return Err(field(ConfigurationErrorCode::InvalidField, "profile")),
        };
        let home_assistant_discovery = match configuration.setting("home_assistant_discovery") {
            Some(ConfigurationValue::Boolean(value)) => *value,
            None => false,
            _ => {
                return Err(field(
                    ConfigurationErrorCode::InvalidField,
                    "home_assistant_discovery",
                ));
            }
        };
        let client_id = self.topics.client_id(&configuration.target_instance_id)?;
        for online in [true, false] {
            if self
                .topics
                .availability_publication(&configuration.target_instance_id, online)
                .payload
                .len()
                > self.runtime.maximum_message_bytes
            {
                return Err(ConfigurationError::new(
                    ConfigurationErrorCode::InvalidField,
                ));
            }
        }
        Ok(MqttSettings {
            endpoint,
            credentials_file,
            target_instance_id: configuration.target_instance_id.clone(),
            configuration_revision: configuration.revision,
            client_id,
            command_principal: uob_contracts::PrincipalId::new(format!(
                "mqtt-target:{}",
                configuration.target_instance_id.as_str()
            ))
            .expect("a validated target identity produces a visible principal"),
            home_assistant_discovery,
            profile,
        })
    }
}

impl<E, P> BridgeTargetFactory<E, P> for MqttTargetFactory
where
    E: serde::Serialize + Send + Sync + 'static,
    P: Clone + serde::de::DeserializeOwned + Send + 'static,
{
    fn kind(&self) -> &'static str {
        MQTT_TARGET_KIND
    }

    fn configuration_schema(&self) -> ConfigurationSchema {
        mqtt_configuration_schema()
    }

    fn validate(
        &self,
        configuration: &TargetConfiguration,
    ) -> Result<ValidatedTargetConfiguration, ConfigurationError> {
        self.settings(configuration)?;
        Ok(ValidatedTargetConfiguration::new(configuration.clone()))
    }

    fn create(
        &self,
        configuration: ValidatedTargetConfiguration,
    ) -> Result<Box<dyn BridgeTarget<E, P>>, ConfigurationError> {
        let settings = self.settings(configuration.configuration())?;
        Ok(Box::new(MqttTarget::new(
            self.topics.clone(),
            settings,
            self.runtime,
        )))
    }
}

/// Credential-free shape presented by the common target catalog.
#[must_use]
pub fn mqtt_configuration_schema() -> ConfigurationSchema {
    ConfigurationSchema {
        fields: vec![
            ConfigurationField {
                name: "broker_url".to_owned(),
                kind: ConfigurationFieldKind::Text,
                required: true,
            },
            ConfigurationField {
                name: "credentials_file".to_owned(),
                kind: ConfigurationFieldKind::CredentialReference,
                required: false,
            },
            ConfigurationField {
                name: "allow_plaintext".to_owned(),
                kind: ConfigurationFieldKind::Boolean,
                required: false,
            },
            ConfigurationField {
                name: "home_assistant_discovery".to_owned(),
                kind: ConfigurationFieldKind::Boolean,
                required: false,
            },
            ConfigurationField {
                name: "profile".to_owned(),
                kind: ConfigurationFieldKind::Text,
                required: false,
            },
        ],
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BrokerEndpoint {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) tls: bool,
}

impl BrokerEndpoint {
    fn parse(value: &str) -> Result<Self, ConfigurationError> {
        let parsed = Url::parse(value)
            .map_err(|_| field(ConfigurationErrorCode::InvalidField, "broker_url"))?;
        let tls = match parsed.scheme() {
            "mqtts" => true,
            "mqtt" => false,
            _ => return Err(field(ConfigurationErrorCode::Unsupported, "broker_url")),
        };
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !matches!(parsed.path(), "" | "/")
        {
            return Err(field(ConfigurationErrorCode::InvalidField, "broker_url"));
        }
        let host = parsed
            .host_str()
            .filter(|host| !host.is_empty())
            .ok_or_else(|| field(ConfigurationErrorCode::InvalidField, "broker_url"))?;
        let port = parsed.port().unwrap_or(if tls { 8883 } else { 1883 });
        Ok(Self {
            host: host.to_owned(),
            port,
            tls,
        })
    }

    pub(crate) fn peer_name(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[derive(Clone)]
pub(crate) struct MqttSettings {
    pub(crate) endpoint: BrokerEndpoint,
    pub(crate) credentials_file: Option<CredentialReference>,
    pub(crate) target_instance_id: uob_contracts::TargetInstanceId,
    pub(crate) configuration_revision: u64,
    pub(crate) client_id: String,
    pub(crate) command_principal: uob_contracts::PrincipalId,
    pub(crate) home_assistant_discovery: bool,
    pub(crate) profile: MqttProfile,
}

fn field(code: ConfigurationErrorCode, name: &'static str) -> ConfigurationError {
    ConfigurationError::field(code, name)
}
