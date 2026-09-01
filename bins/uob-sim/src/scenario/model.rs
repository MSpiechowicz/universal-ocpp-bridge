use std::collections::HashSet;
use std::time::Duration;

use serde::Deserialize;

use super::{FailureCategory, RunFailure, SCHEMA_VERSION};
use crate::{OcppVersion, SimulatorClientConfig};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulatorConfiguration {
    pub schema_version: u16,
    pub stations: Vec<StationDefinition>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StationDefinition {
    pub id: String,
    pub endpoint: String,
    pub ocpp_version: ConfiguredOcppVersion,
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
    #[serde(default)]
    pub reconnect: bool,
    #[serde(default = "default_command_capacity")]
    pub command_capacity: usize,
    #[serde(default = "default_trace_capacity")]
    pub trace_capacity: usize,
    pub credentials_file: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub enum ConfiguredOcppVersion {
    #[serde(rename = "1.6")]
    V1_6,
    #[serde(rename = "2.0.1")]
    V2_0_1,
}

impl From<ConfiguredOcppVersion> for OcppVersion {
    fn from(value: ConfiguredOcppVersion) -> Self {
        match value {
            ConfiguredOcppVersion::V1_6 => Self::V1_6,
            ConfiguredOcppVersion::V2_0_1 => Self::V2_0_1,
        }
    }
}

impl StationDefinition {
    #[must_use]
    pub fn client_config(&self) -> SimulatorClientConfig {
        SimulatorClientConfig {
            endpoint: self.endpoint.clone(),
            version: self.ocpp_version.into(),
            request_timeout: Duration::from_millis(self.request_timeout_ms),
            reconnect: self.reconnect,
            command_capacity: self.command_capacity,
            trace_capacity: self.trace_capacity,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioDefinition {
    pub schema_version: u16,
    pub seed: u64,
    pub steps: Vec<StepDefinition>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepDefinition {
    pub id: String,
    pub station: String,
    pub action: ActionKind,
    pub timeout_ms: u64,
    pub duration_ms: Option<u64>,
    pub expect_message: Option<String>,
    pub expect_event: Option<String>,
    pub expect_detail: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Connect,
    Heartbeat,
    Wait,
    Disconnect,
}

impl ActionKind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Heartbeat => "heartbeat",
            Self::Wait => "wait",
            Self::Disconnect => "disconnect",
        }
    }

    #[must_use]
    pub const fn event(self) -> &'static str {
        match self {
            Self::Connect => "connected",
            Self::Heartbeat => "heartbeat_result",
            Self::Wait => "delay_elapsed",
            Self::Disconnect => "disconnected",
        }
    }
}

/// Parses and validates a versioned simulator configuration.
///
/// # Errors
///
/// Returns a redacted setup failure when the TOML or any configured bound is invalid.
pub fn parse_configuration(input: &str) -> Result<SimulatorConfiguration, RunFailure> {
    let configuration: SimulatorConfiguration = toml::from_str(input).map_err(|_| {
        setup_failure(
            "invalid_configuration_toml",
            "simulator configuration is not valid versioned TOML",
        )
    })?;
    validate_configuration(&configuration)?;
    Ok(configuration)
}

/// Parses and validates a versioned deterministic scenario.
///
/// # Errors
///
/// Returns a redacted setup failure for invalid TOML, versions, actions, or expectations.
pub fn parse_scenario(input: &str) -> Result<ScenarioDefinition, RunFailure> {
    let scenario: ScenarioDefinition = toml::from_str(input).map_err(|_| {
        setup_failure(
            "invalid_scenario_toml",
            "scenario is not valid versioned TOML",
        )
    })?;
    validate_scenario(&scenario)?;
    Ok(scenario)
}

fn validate_configuration(configuration: &SimulatorConfiguration) -> Result<(), RunFailure> {
    validate_version(configuration.schema_version, "configuration")?;
    if configuration.stations.is_empty() {
        return Err(setup_failure(
            "missing_stations",
            "configuration must define at least one station",
        ));
    }
    let mut station_ids = HashSet::new();
    for station in &configuration.stations {
        if station.id.trim().is_empty() || !station_ids.insert(station.id.as_str()) {
            return Err(setup_failure(
                "invalid_station_id",
                "station identifiers must be non-empty and unique",
            ));
        }
        if !(station.endpoint.starts_with("ws://") || station.endpoint.starts_with("wss://")) {
            return Err(setup_failure(
                "invalid_station_endpoint",
                "station endpoints must use ws:// or wss://",
            ));
        }
        if station.request_timeout_ms == 0
            || station.command_capacity == 0
            || station.trace_capacity == 0
        {
            return Err(setup_failure(
                "invalid_station_bound",
                "station timeouts and capacities must be greater than zero",
            ));
        }
    }
    Ok(())
}

fn validate_scenario(scenario: &ScenarioDefinition) -> Result<(), RunFailure> {
    validate_version(scenario.schema_version, "scenario")?;
    if scenario.steps.is_empty() {
        return Err(setup_failure(
            "missing_steps",
            "scenario must define at least one step",
        ));
    }
    let mut step_ids = HashSet::new();
    for step in &scenario.steps {
        if step.id.trim().is_empty() || !step_ids.insert(step.id.as_str()) {
            return Err(setup_failure(
                "invalid_step_id",
                "step identifiers must be non-empty and unique",
            ));
        }
        if step.station.trim().is_empty() || step.timeout_ms == 0 {
            return Err(setup_failure(
                "invalid_step_bound",
                "every step needs a station and a nonzero timeout",
            ));
        }
        validate_step_fields(step)?;
    }
    Ok(())
}

fn validate_step_fields(step: &StepDefinition) -> Result<(), RunFailure> {
    if matches!(step.action, ActionKind::Wait) != step.duration_ms.is_some() {
        return Err(setup_failure(
            "invalid_action_fields",
            "only wait actions require duration_ms",
        ));
    }
    if let Some(event) = &step.expect_event {
        if event != step.action.event() {
            return Err(setup_failure(
                "unsupported_expected_event",
                "expected event is not produced by the selected action",
            ));
        }
    }
    if let Some(message) = &step.expect_message {
        if !matches!(step.action, ActionKind::Heartbeat) || message != "Heartbeat" {
            return Err(setup_failure(
                "unsupported_expected_message",
                "only the Heartbeat wire message is supported by this scenario version",
            ));
        }
    }
    Ok(())
}

fn validate_version(version: u16, document: &str) -> Result<(), RunFailure> {
    if version == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(setup_failure(
            "unsupported_schema_version",
            format!("unsupported {document} schema version"),
        ))
    }
}

fn setup_failure(code: &'static str, message: impl Into<String>) -> RunFailure {
    RunFailure::new(FailureCategory::Setup, code, message)
}

const fn default_request_timeout_ms() -> u64 {
    5_000
}

const fn default_command_capacity() -> usize {
    8
}

const fn default_trace_capacity() -> usize {
    64
}
