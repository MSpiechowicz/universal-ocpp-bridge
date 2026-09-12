use crate::scenario::{
    ScenarioDefinition, SimulatorConfiguration, parse_configuration, parse_scenario,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    net::{IpAddr, SocketAddr},
    path::Path,
};

pub const DOCUMENT_LIMIT: usize = 64 * 1024;
pub const MAX_RUNS: usize = 8;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u16,
    environment: String,
    token_file: String,
    simulator_file: String,
    scenarios: Vec<ScenarioFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioFile {
    id: String,
    path: String,
    #[serde(default)]
    sanitized_import: bool,
}

/// Trusted startup configuration. Fields are private so validation cannot be bypassed.
pub struct ControlConfiguration {
    pub(super) environment: String,
    pub(super) token: String,
    pub(super) bind: SocketAddr,
    pub(super) simulator: SimulatorConfiguration,
    pub(super) scenarios: BTreeMap<String, ScenarioDefinition>,
    pub(super) imports: BTreeSet<String>,
}

impl ControlConfiguration {
    /// Loads a finite catalog of synthetic scenarios, separately from normal `run` configuration.
    ///
    /// # Errors
    /// Rejects production/implicit environments, remote addresses, unsafe peer identities and bounds.
    pub fn load(path: &Path, bind: SocketAddr) -> Result<Self, &'static str> {
        if !bind.ip().is_loopback() || bind.port() == 0 {
            return Err("loopback_control_bind_required");
        }
        let document: Document = toml::from_str(&read_bounded(path, DOCUMENT_LIMIT)?)
            .map_err(|_| "invalid_control_configuration")?;
        if document.schema_version != 1
            || !matches!(document.environment.as_str(), "demo" | "staging")
        {
            return Err("explicit_test_environment_required");
        }
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let token = read_bounded(&parent.join(document.token_file), 128)?
            .trim()
            .to_owned();
        if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("control_token_requires_32_random_bytes_hex");
        }
        let simulator = parse_configuration(&read_bounded(
            &parent.join(document.simulator_file),
            DOCUMENT_LIMIT,
        )?)
        .map_err(|failure| failure.code)?;
        validate_simulator(&simulator, &document.environment)?;
        if document.scenarios.is_empty() || document.scenarios.len() > 16 {
            return Err("catalog_limit");
        }
        let mut scenarios = BTreeMap::new();
        let mut imports = BTreeSet::new();
        for entry in document.scenarios {
            if !valid_id(&entry.id) {
                return Err("invalid_scenario_id");
            }
            let input = read_bounded(&parent.join(entry.path), DOCUMENT_LIMIT)?;
            let scenario = if entry.sanitized_import {
                imports.insert(entry.id.clone());
                super::import::parse(&input, &simulator, &document.environment)?
            } else {
                parse_scenario(&input).map_err(|failure| failure.code)?
            };
            if scenario
                .steps
                .iter()
                .map(|step| u128::from(step.timeout_ms))
                .sum::<u128>()
                > 120_000
                || scenario.steps.len() > 256
                || scenario.steps.iter().any(|step| {
                    !valid_id(&step.id)
                        || step.timeout_ms > 30_000
                        || step.start_delay_ms > 30_000
                        || step.jitter_ms > 30_000
                        || step.duration_ms.is_some_and(|value| value > 30_000)
                        || step
                            .fault
                            .as_ref()
                            .is_some_and(|fault| fault.delay_ms > 30_000)
                        || !simulator
                            .stations
                            .iter()
                            .any(|station| station.id == step.station)
                })
            {
                return Err("scenario_control_bound");
            }
            for station in &simulator.stations {
                if scenario
                    .steps
                    .iter()
                    .filter(|step| step.station == station.id)
                    .count()
                    > station.step_capacity
                {
                    return Err("station_step_capacity_exceeded");
                }
            }
            if scenarios.insert(entry.id, scenario).is_some() {
                return Err("duplicate_scenario_id");
            }
        }
        Ok(Self {
            environment: document.environment,
            token,
            bind,
            simulator,
            scenarios,
            imports,
        })
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(super) fn read_bounded(path: &Path, limit: usize) -> Result<String, &'static str> {
    let file = File::open(path).map_err(|_| "document_unavailable")?;
    if !file
        .metadata()
        .map_err(|_| "document_unavailable")?
        .is_file()
    {
        return Err("regular_document_required");
    }
    let mut text = String::new();
    file.take(limit as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|_| "document_unavailable")?;
    if text.len() > limit {
        return Err("document_limit");
    }
    Ok(text)
}

pub(super) fn validate_simulator(
    simulator: &SimulatorConfiguration,
    environment: &str,
) -> Result<(), &'static str> {
    if simulator.station_capacity > 16 || simulator.stations.len() > 16 {
        return Err("station_limit");
    }
    for station in &simulator.stations {
        let endpoint = url::Url::parse(&station.endpoint).map_err(|_| "invalid_test_peer")?;
        let host = endpoint
            .host_str()
            .ok_or("invalid_test_peer")?
            .trim_matches(['[', ']']);
        let ip: IpAddr = host.parse().map_err(|_| "literal_loopback_peer_required")?;
        if !ip.is_loopback()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || station.credentials_file.is_some()
            || !station.id.starts_with(&format!("{environment}-"))
            || endpoint.path().rsplit('/').next() != Some(station.id.as_str())
        {
            return Err("isolated_test_peer_required");
        }
        if station.id.len() > 64
            || station.step_capacity > 256
            || !(2..=16).contains(&station.command_capacity)
            || station.trace_capacity > 128
            || station.request_timeout_ms > 30_000
            || station.reconnect
        {
            return Err("station_control_bound");
        }
    }
    Ok(())
}
