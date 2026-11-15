//! Charger-initiated OCPP 2.0.1 `NotifyEvent` evidence, journaled as observed facts.
use super::{MONITORING_EVENTS_PER_RECORD_LIMIT_201, is_zero};
use crate::{
    DeviceComponent201, DeviceVariable201, EventNotification201, EventTrigger201, UtcTimestamp,
};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

const ACTUAL_VALUE_MAX_CHARS: usize = 2500;
const TECH_CODE_MAX_CHARS: usize = 50;
const TECH_INFO_MAX_CHARS: usize = 500;
const TRANSACTION_ID_MAX_CHARS: usize = 36;

fn bounded_text<'de, D: Deserializer<'de>, const MAX: usize>(
    deserializer: D,
) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.chars().count() > MAX {
        return Err(de::Error::custom(format!("text exceeds {MAX} characters")));
    }
    Ok(value)
}

fn bounded_optional_text<'de, D: Deserializer<'de>, const MAX: usize>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    if value
        .as_deref()
        .is_some_and(|text| text.chars().count() > MAX)
    {
        return Err(de::Error::custom(format!("text exceeds {MAX} characters")));
    }
    Ok(value)
}

fn bounded_events<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<MonitoringEvent201>, D::Error> {
    let events = Vec::<MonitoringEvent201>::deserialize(deserializer)?;
    if events.len() > MONITORING_EVENTS_PER_RECORD_LIMIT_201 {
        return Err(de::Error::custom(format!(
            "a record holds at most {MONITORING_EVENTS_PER_RECORD_LIMIT_201} events"
        )));
    }
    Ok(events)
}

/// How a `NotifyEvent` sequence number relates to the previous one on the same connection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MonitoringEventOrdering201 {
    /// First message of a series or the expected successor.
    InSequence,
    /// The sequence restarted at zero.
    Restarted,
    /// Messages were skipped; `expected` is the number that should have arrived.
    Gap { expected: u32 },
}

/// Validated native `EventDataType`. The monitor severity is not on the wire and is not
/// retained; look the monitor up through `variable_monitoring_id`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MonitoringEvent201 {
    pub event_id: i32,
    pub timestamp: UtcTimestamp,
    pub trigger: EventTrigger201,
    /// Event ID of the event this one was caused by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<i32>,
    /// Actual value of the variable, at most 2500 characters.
    #[serde(deserialize_with = "bounded_text::<_, ACTUAL_VALUE_MAX_CHARS>")]
    #[schemars(length(max = 2500))]
    pub actual_value: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "bounded_optional_text::<_, TECH_CODE_MAX_CHARS>"
    )]
    #[schemars(length(max = 50))]
    pub tech_code: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "bounded_optional_text::<_, TECH_INFO_MAX_CHARS>"
    )]
    #[schemars(length(max = 500))]
    pub tech_info: Option<String>,
    /// A monitored situation returned to normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared: Option<bool>,
    /// Native transaction identity as reported, not a canonical ID.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "bounded_optional_text::<_, TRANSACTION_ID_MAX_CHARS>"
    )]
    #[schemars(length(max = 36))]
    pub transaction_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variable_monitoring_id: Option<i32>,
    pub notification: EventNotification201,
    pub component: DeviceComponent201,
    pub variable: DeviceVariable201,
}

/// One acknowledged `NotifyEventRequest`, journaled as observed evidence. It never changes
/// configuration, policy or canonical state.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MonitoringEventRecord201 {
    pub generated_at: UtcTimestamp,
    pub seq_no: u32,
    /// The charger announced that more events follow.
    pub tbc: bool,
    pub ordering: MonitoringEventOrdering201,
    /// At most [`MONITORING_EVENTS_PER_RECORD_LIMIT_201`] events, in native order.
    #[serde(deserialize_with = "bounded_events")]
    #[schemars(length(max = 32))]
    pub events: Vec<MonitoringEvent201>,
    /// Events received but not retained because of the record bounds.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub events_omitted: u32,
    /// `NotifyEvent` calls acknowledged but dropped by the flood gate since the previous
    /// durable record.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub shed_calls: u32,
    /// Events carried by the shed calls.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub shed_events: u32,
}
