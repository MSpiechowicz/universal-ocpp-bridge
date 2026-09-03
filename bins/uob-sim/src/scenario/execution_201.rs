use super::{FailureCategory, RunFailure, StationResource, StationState, StepDefinition};
use crate::{OcppVersion, SimulatorAction};

pub(super) fn validate_before(
    state: &StationState,
    step: &StepDefinition,
    action: SimulatorAction,
) -> Result<(), RunFailure> {
    if state.version != OcppVersion::V2_0_1 {
        return Err(failure(
            "wrong_protocol_action",
            "OCPP 2.0.1 charging action used for a different station version",
        ));
    }
    let payload = step.payload.as_ref().expect("validated charging payload");
    if action == SimulatorAction::BootNotification {
        return Ok(());
    }
    if !state.registered {
        return Err(failure(
            "station_not_registered",
            "charging calls require an accepted BootNotification",
        ));
    }
    if action == SimulatorAction::Authorize {
        return Ok(());
    }
    if action == SimulatorAction::StatusNotification {
        if state.resource(status_resource(payload)?).is_none() {
            return Err(failure(
                "unknown_evse_connector",
                "EVSE and connector pair is not configured",
            ));
        }
        return Ok(());
    }

    validate_event_type(payload, action)?;
    let transaction_id = transaction_id(payload)?;
    let sequence = sequence(payload)?;
    let resource = evse_resource(payload)?;
    let current = state.resource(resource).ok_or_else(|| {
        failure(
            "unknown_evse_connector",
            "EVSE and connector pair is not configured",
        )
    })?;

    if action == SimulatorAction::StartTransaction {
        let token = payload
            .pointer("/idToken/idToken")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| failure("missing_id_token", "start requires idToken"))?;
        if !state.is_authorized(token) {
            return Err(failure(
                "not_authorized",
                "transaction start requires a previously accepted authorization",
            ));
        }
        if current.transaction_id.is_some() {
            return Err(failure(
                "transaction_already_active",
                "EVSE connector already has an active transaction",
            ));
        }
        if sequence != 0 {
            return Err(failure(
                "out_of_order_transaction_sequence",
                "a started transaction must use sequence zero",
            ));
        }
    } else {
        if current.transaction_id.as_deref() != Some(transaction_id) {
            return Err(failure(
                "transaction_not_active",
                "transaction event does not match the active EVSE connector transaction",
            ));
        }
        let expected = current.last_sequence.map_or(0, |value| value + 1);
        if sequence < expected {
            return Err(failure(
                "duplicate_transaction_sequence",
                "transaction sequence number was already observed",
            ));
        }
        if sequence > expected {
            return Err(failure(
                "out_of_order_transaction_sequence",
                "transaction sequence number skipped an expected event",
            ));
        }
    }
    if action == SimulatorAction::MeterValues {
        validate_meter_quality(payload)?;
    }
    Ok(())
}

pub(super) fn apply_after(
    state: &mut StationState,
    step: &StepDefinition,
    action: SimulatorAction,
    response: &serde_json::Value,
) -> Result<(), RunFailure> {
    let payload = step.payload.as_ref().expect("validated charging payload");
    if action == SimulatorAction::BootNotification
        && response.get("status").and_then(serde_json::Value::as_str) == Some("Accepted")
    {
        state.registered = true;
    }
    if action == SimulatorAction::Authorize
        && response
            .pointer("/idTokenInfo/status")
            .and_then(serde_json::Value::as_str)
            == Some("Accepted")
    {
        state.authorize(
            payload
                .pointer("/idToken/idToken")
                .and_then(serde_json::Value::as_str)
                .expect("typed authorization payload"),
        );
    }
    if !matches!(
        action,
        SimulatorAction::StartTransaction
            | SimulatorAction::MeterValues
            | SimulatorAction::StopTransaction
    ) {
        return Ok(());
    }
    if action == SimulatorAction::StartTransaction
        && response
            .pointer("/idTokenInfo/status")
            .and_then(serde_json::Value::as_str)
            != Some("Accepted")
    {
        return Ok(());
    }

    let resource = evse_resource(payload)?;
    let sequence = sequence(payload)?;
    if action == SimulatorAction::StartTransaction {
        state
            .start_transaction(resource, transaction_id(payload)?)
            .map_err(|_| transition_failure())?;
        state.record_local_effect();
    }
    state
        .record_sequence(resource, sequence)
        .map_err(|_| transition_failure())?;
    if action == SimulatorAction::StopTransaction {
        state
            .stop_transaction(resource)
            .map_err(|_| transition_failure())?;
    }
    Ok(())
}

fn validate_event_type(
    payload: &serde_json::Value,
    action: SimulatorAction,
) -> Result<(), RunFailure> {
    let expected = match action {
        SimulatorAction::StartTransaction => "Started",
        SimulatorAction::MeterValues => "Updated",
        SimulatorAction::StopTransaction => "Ended",
        _ => return Ok(()),
    };
    if payload.get("eventType").and_then(serde_json::Value::as_str) == Some(expected) {
        Ok(())
    } else {
        Err(failure(
            "invalid_transaction_event_type",
            "transaction action does not match its OCPP 2.0.1 eventType",
        ))
    }
}

fn validate_meter_quality(payload: &serde_json::Value) -> Result<(), RunFailure> {
    let values = payload
        .get("meterValue")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| failure("missing_meter_values", "meter update requires meterValue"))?;
    let complete = !values.is_empty()
        && values.iter().all(|meter| {
            meter
                .get("timestamp")
                .and_then(serde_json::Value::as_str)
                .is_some()
                && meter
                    .get("sampledValue")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|samples| {
                        !samples.is_empty()
                            && samples.iter().all(|sample| {
                                sample.get("value").is_some()
                                    && sample.get("context").is_some()
                                    && sample.get("measurand").is_some()
                                    && sample.get("phase").is_some()
                                    && sample.get("location").is_some()
                                    && sample.pointer("/unitOfMeasure/unit").is_some()
                            })
                    })
        });
    if complete {
        Ok(())
    } else {
        Err(failure(
            "missing_meter_data_quality",
            "meter samples require timestamp, context, measurand, phase, location, and unit",
        ))
    }
}

fn evse_resource(payload: &serde_json::Value) -> Result<StationResource, RunFailure> {
    let evse_id = positive_id(payload, "/evse/id")?;
    let connector_id = positive_id(payload, "/evse/connectorId")?;
    Ok(StationResource::EvseConnector {
        evse_id,
        connector_id,
    })
}

fn status_resource(payload: &serde_json::Value) -> Result<StationResource, RunFailure> {
    let evse_id = positive_id(payload, "/evseId")?;
    let connector_id = positive_id(payload, "/connectorId")?;
    Ok(StationResource::EvseConnector {
        evse_id,
        connector_id,
    })
}

fn positive_id(payload: &serde_json::Value, pointer: &str) -> Result<u16, RunFailure> {
    payload
        .pointer(pointer)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            failure(
                "invalid_evse_connector",
                "EVSE and connector identifiers must be positive integers",
            )
        })
}

fn transaction_id(payload: &serde_json::Value) -> Result<&str, RunFailure> {
    payload
        .pointer("/transactionInfo/transactionId")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| failure("missing_transaction_id", "transactionId is required"))
}

fn sequence(payload: &serde_json::Value) -> Result<i64, RunFailure> {
    payload
        .get("seqNo")
        .and_then(serde_json::Value::as_i64)
        .filter(|value| *value >= 0)
        .ok_or_else(|| failure("invalid_transaction_sequence", "seqNo must be nonnegative"))
}

fn transition_failure() -> RunFailure {
    failure(
        "invalid_transaction_transition",
        "could not apply OCPP 2.0.1 transaction transition",
    )
}

fn failure(code: &'static str, message: &'static str) -> RunFailure {
    RunFailure::new(FailureCategory::Assertion, code, message)
}
