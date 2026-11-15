//! Charger-initiated OCPP 2.0.1 `NotifyEvent` (N07, N08): pinned-schema and native-semantic
//! decoding of one event notification into an observed fact. Nothing here changes configuration,
//! policy or commands. Events beyond the journal record bound are the application gate's to shed,
//! so every event of the CALL is decoded.
use crate::{
    DecodeError, DecodeErrorKind,
    command_registry::monitoring201::{
        NOTIFY_EVENT_SCHEMA,
        parse::{int32, optional_int32, replace_controls},
        valid_schema,
    },
    v201::remote_control::device_model_values::{component, variable},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uob_application::{ChargerObservation, NotifyEventObservation201};
use uob_contracts::{
    DeviceComponent201, DeviceVariable201, EventNotification201, EventTrigger201,
    MonitoringEvent201, ProtocolEdition, UtcTimestamp,
};

const ACTUAL_VALUE_MAX_CHARS: usize = 2500;
const TECH_CODE_MAX_CHARS: usize = 50;
const TECH_INFO_MAX_CHARS: usize = 500;
const TRANSACTION_ID_MAX_CHARS: usize = 36;

fn invalid() -> DecodeError {
    DecodeError::new(ProtocolEdition::Ocpp201, DecodeErrorKind::InvalidPayload)
}

/// Decodes one `NotifyEvent` CALL. The CSMS answers every schema-valid notification with the
/// empty response (N07.FR.03, N08.FR.02), and a refusal would only make the charger retry the
/// queued event, so beyond the pinned schema only what the typed record cannot represent is
/// refused: integers outside 32 bits, a timestamp that is not an instant, and non-positive EVSE
/// or connector identities. Control characters in charger text become U+FFFD, and a missing
/// `variableMonitoringId` stays absent whatever the notification type. `customData` is never
/// retained, and no charger text reaches logs or traces from here.
pub(crate) fn observation(payload: &Value) -> Result<ChargerObservation, DecodeError> {
    if !valid_schema(NOTIFY_EVENT_SCHEMA, payload) {
        return Err(invalid());
    }
    let events = payload["eventData"].as_array().ok_or_else(invalid)?;

    Ok(ChargerObservation::MonitoringEvents201(
        NotifyEventObservation201 {
            generated_at: timestamp(&payload["generatedAt"])?,
            seq_no: payload["seqNo"]
                .as_u64()
                .and_then(|sequence| u32::try_from(sequence).ok())
                .ok_or_else(invalid)?,
            tbc: payload.get("tbc").and_then(Value::as_bool).unwrap_or(false),
            events: events.iter().map(event).collect::<Result<Vec<_>, _>>()?,
        },
    ))
}

fn event(raw: &Value) -> Result<MonitoringEvent201, DecodeError> {
    let notification =
        EventNotification201::deserialize(&raw["eventNotificationType"]).map_err(|_| invalid())?;
    let variable_monitoring_id =
        optional_int32(raw.get("variableMonitoringId")).map_err(|_| invalid())?;

    Ok(MonitoringEvent201 {
        event_id: int32(&raw["eventId"]).ok_or_else(invalid)?,
        timestamp: timestamp(&raw["timestamp"])?,
        trigger: EventTrigger201::deserialize(&raw["trigger"]).map_err(|_| invalid())?,
        cause: optional_int32(raw.get("cause")).map_err(|_| invalid())?,
        actual_value: text(&raw["actualValue"], ACTUAL_VALUE_MAX_CHARS)?,
        tech_code: optional_text(raw.get("techCode"), TECH_CODE_MAX_CHARS)?,
        tech_info: optional_text(raw.get("techInfo"), TECH_INFO_MAX_CHARS)?,
        cleared: match raw.get("cleared") {
            None => None,
            Some(flag) => Some(flag.as_bool().ok_or_else(invalid)?),
        },
        transaction_id: optional_text(raw.get("transactionId"), TRANSACTION_ID_MAX_CHARS)?,
        variable_monitoring_id,
        notification,
        component: component(&raw["component"])
            .map(sanitized_component)
            .ok_or_else(invalid)?,
        variable: variable(&raw["variable"])
            .map(sanitized_variable)
            .ok_or_else(invalid)?,
    })
}

fn sanitized_component(mut component: DeviceComponent201) -> DeviceComponent201 {
    component.name = replace_controls(&component.name);
    component.instance = component.instance.as_deref().map(replace_controls);

    component
}

fn sanitized_variable(mut variable: DeviceVariable201) -> DeviceVariable201 {
    variable.name = replace_controls(&variable.name);
    variable.instance = variable.instance.as_deref().map(replace_controls);

    variable
}

fn timestamp(value: &Value) -> Result<UtcTimestamp, DecodeError> {
    UtcTimestamp::deserialize(value).map_err(|_| invalid())
}

fn text(value: &Value, maximum: usize) -> Result<String, DecodeError> {
    value
        .as_str()
        .filter(|text| text.chars().count() <= maximum)
        .map(replace_controls)
        .ok_or_else(invalid)
}

fn optional_text(value: Option<&Value>, maximum: usize) -> Result<Option<String>, DecodeError> {
    value.map(|value| text(value, maximum)).transpose()
}

/// The exact native response: the empty `NotifyEventResponse` object (N07.FR.03, N08.FR.02).
#[must_use]
pub fn response() -> Value {
    json!({})
}
