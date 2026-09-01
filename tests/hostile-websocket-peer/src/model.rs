use std::collections::HashSet;
use std::error::Error;
use std::fmt::{Display, Formatter};

use serde::Deserialize;

use crate::SCHEMA_VERSION;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub schema_version: u16,
    pub endpoint: String,
    pub subprotocol: String,
    #[serde(default = "default_max_outbound_bytes")]
    pub max_outbound_bytes: usize,
    #[serde(default = "default_max_inbound_bytes")]
    pub max_inbound_bytes: usize,
    #[serde(default = "default_observation_capacity")]
    pub observation_capacity: usize,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub action: Action,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    pub delay_ms: Option<u64>,
    pub payload: Option<Payload>,
    pub expect: Option<ExpectedFrame>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Connect,
    Send,
    Receive,
    Delay,
    Disconnect,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum Payload {
    Text { value: String },
    BinaryHex { value: String },
    RepeatedText { byte: char, bytes: usize },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum ExpectedFrame {
    Text {
        #[serde(default)]
        contains: Option<String>,
        #[serde(default)]
        json_pointer: Option<String>,
        #[serde(default)]
        equals: Option<serde_json::Value>,
    },
    Binary {
        bytes: Option<usize>,
    },
    Close {
        code: Option<u16>,
    },
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioError {
    pub code: &'static str,
    pub message: &'static str,
}

impl Display for ScenarioError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl Error for ScenarioError {}

/// Parses and validates a versioned adversarial scenario without echoing payload contents.
///
/// # Errors
///
/// Returns a stable, redacted error when TOML or a configured bound is invalid.
pub fn parse_scenario(input: &str) -> Result<Scenario, ScenarioError> {
    let scenario: Scenario = toml::from_str(input).map_err(|_| {
        error(
            "invalid_scenario_toml",
            "adversarial scenario is not valid versioned TOML",
        )
    })?;
    validate(&scenario)?;
    Ok(scenario)
}

fn validate(scenario: &Scenario) -> Result<(), ScenarioError> {
    if scenario.schema_version != SCHEMA_VERSION {
        return Err(error(
            "unsupported_schema_version",
            "adversarial scenario schema version is unsupported",
        ));
    }
    if !scenario.endpoint.starts_with("ws://") && !scenario.endpoint.starts_with("wss://") {
        return Err(error(
            "invalid_endpoint",
            "endpoint must use ws:// or wss://",
        ));
    }
    if scenario.subprotocol.trim().is_empty()
        || scenario.max_outbound_bytes == 0
        || scenario.max_inbound_bytes == 0
        || scenario.observation_capacity == 0
        || scenario.steps.is_empty()
    {
        return Err(error(
            "invalid_bound",
            "scenario bounds and subprotocol must be nonzero",
        ));
    }
    let mut ids = HashSet::new();
    for step in &scenario.steps {
        if step.id.trim().is_empty() || !ids.insert(step.id.as_str()) || step.timeout_ms == 0 {
            return Err(error(
                "invalid_step",
                "step IDs must be unique and bounds nonzero",
            ));
        }
        let fields_valid = match step.action {
            Action::Connect | Action::Disconnect => {
                step.payload.is_none() && step.expect.is_none() && step.delay_ms.is_none()
            }
            Action::Send => {
                step.payload.is_some() && step.expect.is_none() && step.delay_ms.is_none()
            }
            Action::Receive => {
                step.payload.is_none() && step.expect.is_some() && step.delay_ms.is_none()
            }
            Action::Delay => {
                step.payload.is_none() && step.expect.is_none() && step.delay_ms.is_some()
            }
        };
        if !fields_valid {
            return Err(error(
                "invalid_step_fields",
                "step fields do not match the selected action",
            ));
        }
        if let Some(Payload::RepeatedText { byte, bytes }) = &step.payload
            && (!byte.is_ascii() || *bytes == 0)
        {
            return Err(error(
                "invalid_repeated_payload",
                "repeated text requires one ASCII byte and a nonzero size",
            ));
        }
        if let Some(Payload::BinaryHex { value }) = &step.payload
            && (value.len() % 2 != 0 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(error(
                "invalid_hex_payload",
                "binary payload must be even-length hex",
            ));
        }
        if let Some(ExpectedFrame::Text {
            json_pointer,
            equals,
            ..
        }) = &step.expect
            && (json_pointer.is_some() != equals.is_some()
                || json_pointer
                    .as_ref()
                    .is_some_and(|path| !path.starts_with('/')))
        {
            return Err(error(
                "invalid_json_expectation",
                "JSON expectations require a pointer beginning with / and an equals value",
            ));
        }
    }
    Ok(())
}

const fn error(code: &'static str, message: &'static str) -> ScenarioError {
    ScenarioError { code, message }
}

const fn default_max_outbound_bytes() -> usize {
    2 * 1024 * 1024
}
const fn default_max_inbound_bytes() -> usize {
    256 * 1024
}
const fn default_observation_capacity() -> usize {
    128
}
const fn default_timeout_ms() -> u64 {
    5_000
}
