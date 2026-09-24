//! Independent EMS consumer: only MQTT 3.1.1, TLS, JSON and published contract schemas.
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use std::future::Future;
use std::{fmt::Write as _, path::Path, sync::LazyLock};

#[path = "exercise.rs"]
mod exercise;
#[path = "session.rs"]
mod session;
#[path = "wire.rs"]
mod wire;

pub type Result<T, E = Error> = std::result::Result<T, E>;
#[derive(Debug)]
pub struct Error(String);
impl Error {
    pub fn from_message(message: &str) -> Self {
        Self(message.to_owned())
    }
    fn new(message: &str) -> Self {
        Self::from_message(message)
    }
    fn owned(message: String) -> Self {
        Self(message)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for Error {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Demo {
    pub bridge_id: String,
    pub scenario: Vec<Scenario>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub protocol: String,
    pub station: String,
    pub point_id: String,
    pub unit: String,
    pub value: String,
    pub observed_at: String,
    #[serde(default)]
    pub source_time: Option<String>,
    #[serde(default)]
    pub measurement_original_value: Option<String>,
    #[serde(default)]
    pub measurement_original_unit: Option<String>,
    pub freshness: Value,
    pub quality: Value,
    pub authorization_reference: String,
    #[serde(default)]
    pub expected_current: Option<bool>,
}
#[derive(Serialize)]
pub struct CatalogEvidence {
    pub descriptor: bool,
    pub exact_value: bool,
    pub retained_state: bool,
}
#[derive(Serialize)]
pub struct ScenarioEvidence {
    pub protocol: String,
    pub station: String,
    #[serde(flatten)]
    pub catalog: CatalogEvidence,
    pub current_at_receive: bool,
    pub current_after_reconnect: Option<bool>,
    #[serde(flatten)]
    pub commands: exercise::CommandEvidence,
}
#[derive(Serialize)]
pub struct Evidence {
    pub status: &'static str,
    pub connected: bool,
    pub target_online: bool,
    pub subscriptions_acknowledged: usize,
    pub consumer_reconnects: usize,
    pub retained_after_reconnect: bool,
    pub scenarios: Vec<ScenarioEvidence>,
}

static POINT_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    compile(include_str!(
        "../../crates/contracts/schemas/v1.0/data-point-descriptor.schema.json"
    ))
});
static VALUE_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    compile(include_str!(
        "../../crates/contracts/schemas/v1.0/data-point-value.schema.json"
    ))
});
static STATE_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    compile(include_str!(
        "../../crates/contracts/schemas/v1.0/station-snapshot.schema.json"
    ))
});
static RESULT_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    compile(include_str!(
        "../../crates/contracts/schemas/v1.0/command-result.schema.json"
    ))
});
static EVENT_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    compile(include_str!(
        "../../crates/contracts/schemas/v1.0/event-envelope.schema.json"
    ))
});
fn compile(source: &str) -> jsonschema::Validator {
    let contract: Value = serde_json::from_str(source).expect("checked-in contract JSON");
    jsonschema::options()
        .offline()
        .build(&contract)
        .expect("checked-in contract schema")
}
fn document(message: &rumqttc::Publish, contract: &jsonschema::Validator) -> Result<Value> {
    let value =
        serde_json::from_slice(&message.payload).map_err(|_| Error::new("invalid MQTT JSON"))?;
    contract
        .validate(&value)
        .map_err(|error| Error::owned(format!("canonical schema violation: {error}")))?;
    Ok(value)
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| Error::owned(format!("missing {key}")))
}
fn encoded(value: &str) -> String {
    let mut text = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            text.push(char::from(byte));
        } else {
            write!(text, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    text
}

fn current(value: &Value) -> Result<bool> {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    let observed = OffsetDateTime::parse(field(value, "observed_at")?, &Rfc3339)
        .map_err(|_| Error::new("invalid observation time"))?;
    let now = OffsetDateTime::now_utc();
    let until = value["freshness"]["valid_until"]
        .as_str()
        .map(|text| {
            OffsetDateTime::parse(text, &Rfc3339).map_err(|_| Error::new("invalid validity time"))
        })
        .transpose()?;
    Ok(value["quality"]["level"] == "good"
        && value["freshness"]["status"] == "fresh"
        && observed <= now
        && until.is_some_and(|end| end > now))
}

/// Read-only unless `exercise` is explicitly enabled. Broker ACK proves only broker receipt;
/// result and transaction-state observations are reported independently.
pub async fn run(
    broker_url: &str,
    ca_file: &Path,
    username: &str,
    password_file: &Path,
    demo: &Demo,
    exercise: bool,
    allow_remote_exercise: bool,
) -> Result<Evidence> {
    session::run_session(
        session::Connection {
            broker_url,
            ca_file,
            username,
            password_file,
        },
        demo,
        exercise,
        allow_remote_exercise,
        std::future::ready(()),
        false,
    )
    .await
}

/// An independent read-only consumer stays connected through a test-owned broker outage.
/// The callback must actually interrupt the broker and restore it; evidence is produced
/// only after a fresh CONNACK, SUBACKs, and retained state/catalog from the resumed broker.
#[cfg(test)]
pub async fn run_with_outage<F: Future<Output = ()>>(
    broker_url: &str,
    ca_file: &Path,
    username: &str,
    password_file: &Path,
    demo: &Demo,
    outage: F,
) -> Result<Evidence> {
    session::run_session(
        session::Connection {
            broker_url,
            ca_file,
            username,
            password_file,
        },
        demo,
        false,
        false,
        outage,
        true,
    )
    .await
}
