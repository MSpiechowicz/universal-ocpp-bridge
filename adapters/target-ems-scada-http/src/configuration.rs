use std::net::SocketAddr;

use uob_application::{
    BridgeTarget, BridgeTargetFactory, ConfigurationError, ConfigurationErrorCode,
    ConfigurationField, ConfigurationFieldKind, ConfigurationSchema, ConfigurationValue,
    CredentialReference, TargetConfiguration, ValidatedTargetConfiguration,
};
use uob_contracts::{Environment, TargetInstanceId};

use crate::target::EmsScadaHttpTarget;

pub(crate) use self::credentials::resolve_credentials;
pub use self::credentials::{IntegrationCredentials, IntegrationPrincipal};

pub(crate) mod bounded_file;
mod credentials;
#[cfg(test)]
mod tests;

/// Stable registry kind for the direct EMS/SCADA HTTP integration target.
pub const EMS_SCADA_HTTP_TARGET_KIND: &str = "ems-scada.http";

/// Every integration route lives under this single versioned prefix.
///
/// Administrative configuration, debug capture, and simulator controls stay on the independent
/// management API and are deliberately unreachable from this listener.
pub const INTEGRATION_PATH_PREFIX: &str = "/bridge/v1";

/// Adapter-owned runtime bounds fixed by the composition root, never by integration clients.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmsScadaHttpRuntimeOptions {
    /// Largest canonical JSON payload the integration mapping accepts.
    pub maximum_message_bytes: usize,
    /// Largest number of host deliveries the adapter processes concurrently.
    pub maximum_in_flight_deliveries: usize,
    /// Largest number of integration-originated commands awaiting host admission.
    pub maximum_in_flight_commands: usize,
    /// Largest number of integration clients served concurrently.
    pub maximum_concurrent_clients: usize,
    /// Largest accepted request body.
    pub maximum_request_bytes: usize,
}

impl Default for EmsScadaHttpRuntimeOptions {
    fn default() -> Self {
        Self {
            maximum_message_bytes: 256 * 1024,
            maximum_in_flight_deliveries: 16,
            maximum_in_flight_commands: 8,
            maximum_concurrent_clients: 32,
            maximum_request_bytes: 64 * 1024,
        }
    }
}

impl EmsScadaHttpRuntimeOptions {
    fn validate(self) -> Result<Self, ConfigurationError> {
        if self.maximum_message_bytes == 0
            || self.maximum_in_flight_deliveries == 0
            || self.maximum_in_flight_commands == 0
            || self.maximum_concurrent_clients == 0
            || self.maximum_request_bytes == 0
        {
            return Err(ConfigurationError::new(
                ConfigurationErrorCode::InvalidField,
            ));
        }
        Ok(self)
    }
}

/// Network-free factory bound to the trusted process environment.
///
/// Construction allocates no listener, socket, or task: an unselected target costs nothing.
#[derive(Clone)]
pub struct EmsScadaHttpTargetFactory {
    environment: Environment,
    runtime: EmsScadaHttpRuntimeOptions,
}

impl EmsScadaHttpTargetFactory {
    /// Creates a factory for one trusted runtime environment.
    #[must_use]
    pub fn new(environment: Environment) -> Self {
        Self {
            environment,
            runtime: EmsScadaHttpRuntimeOptions::default(),
        }
    }

    /// Replaces fixed process-owned bounds, primarily for constrained deployments and tests.
    ///
    /// # Errors
    ///
    /// Rejects zero bounds.
    pub fn with_runtime_options(
        mut self,
        runtime: EmsScadaHttpRuntimeOptions,
    ) -> Result<Self, ConfigurationError> {
        self.runtime = runtime.validate()?;
        Ok(self)
    }

    fn settings(
        &self,
        configuration: &TargetConfiguration,
    ) -> Result<EmsScadaHttpSettings, ConfigurationError> {
        ems_scada_http_configuration_schema().validate_shape(configuration)?;
        let Some(ConfigurationValue::Text(listen_addr)) = configuration.setting("listen_addr")
        else {
            return Err(field(ConfigurationErrorCode::MissingField, "listen_addr"));
        };
        let listen_address: SocketAddr = listen_addr
            .parse()
            .map_err(|_| field(ConfigurationErrorCode::InvalidField, "listen_addr"))?;
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
        let tls_configured = tls_configured(configuration)?;
        let remote_access_enabled = match configuration.setting("remote_access_enabled") {
            Some(ConfigurationValue::Boolean(value)) => *value,
            None => false,
            _ => {
                return Err(field(
                    ConfigurationErrorCode::InvalidField,
                    "remote_access_enabled",
                ));
            }
        };

        let settings = EmsScadaHttpSettings {
            listen_address,
            credentials_file,
            tls_configured,
            remote_access_enabled,
            target_instance_id: configuration.target_instance_id.clone(),
        };
        settings.validate_listener_policy(self.environment)?;
        Ok(settings)
    }
}

/// Validates that a TLS server identity is referenced as a complete certificate/key pair.
///
/// The references are checked for shape only. This build terminates no TLS, so it never opens
/// or parses the referenced material.
fn tls_configured(configuration: &TargetConfiguration) -> Result<bool, ConfigurationError> {
    let certificate = credential(configuration, "tls_certificate_file")?;
    let private_key = credential(configuration, "tls_private_key_file")?;
    match (certificate, private_key) {
        (Some(_), Some(_)) => Ok(true),
        (None, None) => Ok(false),
        (Some(_), None) => Err(field(
            ConfigurationErrorCode::MissingField,
            "tls_private_key_file",
        )),
        (None, Some(_)) => Err(field(
            ConfigurationErrorCode::MissingField,
            "tls_certificate_file",
        )),
    }
}

fn credential(
    configuration: &TargetConfiguration,
    name: &'static str,
) -> Result<Option<CredentialReference>, ConfigurationError> {
    match configuration.setting(name) {
        Some(ConfigurationValue::CredentialReference(value)) => Ok(Some(value.clone())),
        None => Ok(None),
        _ => Err(field(ConfigurationErrorCode::InvalidField, name)),
    }
}

impl<E, P> BridgeTargetFactory<E, P> for EmsScadaHttpTargetFactory
where
    E: Send + Sync + 'static,
    P: Send + 'static,
{
    fn kind(&self) -> &'static str {
        EMS_SCADA_HTTP_TARGET_KIND
    }

    fn configuration_schema(&self) -> ConfigurationSchema {
        ems_scada_http_configuration_schema()
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
        Ok(Box::new(EmsScadaHttpTarget::new(settings, self.runtime)))
    }
}

/// Credential-free shape presented by the common target catalog.
#[must_use]
pub fn ems_scada_http_configuration_schema() -> ConfigurationSchema {
    ConfigurationSchema {
        fields: vec![
            ConfigurationField {
                name: "listen_addr".to_owned(),
                kind: ConfigurationFieldKind::Text,
                required: true,
            },
            ConfigurationField {
                name: "credentials_file".to_owned(),
                kind: ConfigurationFieldKind::CredentialReference,
                required: false,
            },
            ConfigurationField {
                name: "tls_certificate_file".to_owned(),
                kind: ConfigurationFieldKind::CredentialReference,
                required: false,
            },
            ConfigurationField {
                name: "tls_private_key_file".to_owned(),
                kind: ConfigurationFieldKind::CredentialReference,
                required: false,
            },
            ConfigurationField {
                name: "remote_access_enabled".to_owned(),
                kind: ConfigurationFieldKind::Boolean,
                required: false,
            },
        ],
    }
}

/// Validated listener settings for one configured integration target instance.
#[derive(Clone)]
pub(crate) struct EmsScadaHttpSettings {
    pub(crate) listen_address: SocketAddr,
    pub(crate) credentials_file: Option<CredentialReference>,
    /// Whether a complete TLS server identity was referenced by configuration.
    pub(crate) tls_configured: bool,
    pub(crate) remote_access_enabled: bool,
    pub(crate) target_instance_id: TargetInstanceId,
}

impl EmsScadaHttpSettings {
    /// Rejects an exposed listener that is not explicitly enabled, encrypted, and authenticated.
    fn validate_listener_policy(&self, environment: Environment) -> Result<(), ConfigurationError> {
        if environment == Environment::Production && self.credentials_file.is_none() {
            return Err(field(
                ConfigurationErrorCode::MissingField,
                "credentials_file",
            ));
        }
        if self.listen_address.ip().is_loopback() {
            return Ok(());
        }
        if !self.remote_access_enabled {
            return Err(field(
                ConfigurationErrorCode::MissingField,
                "remote_access_enabled",
            ));
        }
        if !self.tls_configured {
            return Err(field(
                ConfigurationErrorCode::MissingField,
                "tls_certificate_file",
            ));
        }
        if self.credentials_file.is_none() {
            return Err(field(
                ConfigurationErrorCode::MissingField,
                "credentials_file",
            ));
        }
        Ok(())
    }
}

fn field(code: ConfigurationErrorCode, name: &'static str) -> ConfigurationError {
    ConfigurationError::field(code, name)
}
