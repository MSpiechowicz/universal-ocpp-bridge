use super::execution::failure;
use super::{RunFailure, StationResource, StationState, StepDefinition};
use crate::SimulatorAction;
pub(super) fn validate_before(
    state: &StationState,
    step: &StepDefinition,
    action: SimulatorAction,
) -> Result<(), RunFailure> {
    if state.version != crate::OcppVersion::V1_6 {
        return Err(failure(
            "wrong_protocol_action",
            "OCPP 1.6 charging action used for a different station version",
        ));
    }
    let payload = step.payload.as_ref().expect("validated charging payload");
    if action != SimulatorAction::BootNotification && !state.registered {
        return Err(failure(
            "station_not_registered",
            "charging calls require an accepted BootNotification",
        ));
    }
    if matches!(
        action,
        SimulatorAction::MeterValues | SimulatorAction::StopTransaction
    ) {
        validate_active_transaction(state, payload, action)?;
    }
    if action == SimulatorAction::StartTransaction {
        let id_tag = payload
            .get("idTag")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| failure("missing_id_tag", "start requires idTag"))?;
        if !state.is_authorized(id_tag)
            && !state.local.as_ref().is_some_and(|local| {
                local.authorize_offline(id_tag, time::OffsetDateTime::now_utc())
            })
        {
            return Err(failure(
                "not_authorized",
                "transaction start requires a previously accepted authorization",
            ));
        }
        let resource = connector_resource(payload)?;
        let current = state
            .resource(resource)
            .ok_or_else(|| failure("unknown_connector", "connector is not configured"))?;
        if current.transaction_id.is_some() {
            return Err(failure(
                "transaction_already_active",
                "connector already has an active transaction",
            ));
        }
    }
    Ok(())
}

fn validate_active_transaction(
    state: &StationState,
    payload: &serde_json::Value,
    action: SimulatorAction,
) -> Result<(), RunFailure> {
    let transaction_id = payload
        .get("transactionId")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| failure("missing_transaction_id", "transactionId is required"))?
        .to_string();
    let resource = if action == SimulatorAction::MeterValues {
        connector_resource(payload)?
    } else {
        active_resource(state, &transaction_id)?
    };
    if state
        .resource(resource)
        .and_then(|item| item.transaction_id.as_deref())
        != Some(transaction_id.as_str())
    {
        return Err(failure(
            "transaction_not_active",
            "metering and stop calls require the matching active transaction",
        ));
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
            .pointer("/idTagInfo/status")
            .and_then(serde_json::Value::as_str)
            == Some("Accepted")
    {
        state.authorize(
            payload["idTag"]
                .as_str()
                .expect("typed authorization payload"),
        );
    }
    if action == SimulatorAction::Authorize
        && response
            .pointer("/idTagInfo/status")
            .and_then(serde_json::Value::as_str)
            != Some("Accepted")
    {
        state.deny_authorization(
            payload["idTag"]
                .as_str()
                .expect("typed authorization payload"),
        );
    }
    if action == SimulatorAction::StartTransaction
        && response
            .pointer("/idTagInfo/status")
            .and_then(serde_json::Value::as_str)
            == Some("Accepted")
    {
        let transaction_id = response
            .get("transactionId")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| {
                failure(
                    "missing_transaction_id",
                    "accepted start omitted transactionId",
                )
            })?;
        state
            .start_transaction(connector_resource(payload)?, transaction_id.to_string())
            .map_err(|_| {
                failure(
                    "invalid_transaction_transition",
                    "could not start transaction",
                )
            })?;
        state.record_local_effect();
    }
    if action == SimulatorAction::StopTransaction {
        let transaction_id = payload["transactionId"]
            .as_i64()
            .expect("validated transactionId")
            .to_string();
        state
            .stop_transaction(active_resource(state, &transaction_id)?)
            .map_err(|_| {
                failure(
                    "invalid_transaction_transition",
                    "could not stop transaction",
                )
            })?;
    }
    Ok(())
}

fn connector_resource(payload: &serde_json::Value) -> Result<StationResource, RunFailure> {
    let connector_id = payload
        .get("connectorId")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| {
            failure(
                "invalid_connector",
                "connectorId must be a positive integer",
            )
        })?;
    Ok(StationResource::Connector { connector_id })
}

fn active_resource(
    state: &StationState,
    transaction_id: &str,
) -> Result<StationResource, RunFailure> {
    state
        .resource_for_transaction(transaction_id)
        .ok_or_else(|| failure("transaction_not_active", "transaction is not active"))
}
