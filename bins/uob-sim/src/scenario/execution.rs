use std::sync::Arc;
use std::time::Duration;

use super::{
    ActionKind, CommandAdmission, FailureCategory, FaultKind, RunFailure, ScenarioClock,
    ScenarioConnector, StationDefinition, StationResource, StationState, StepDefinition,
};
use crate::{ProtocolClient, RemoteCommandKind, SimulatorAction, SimulatorCall};

pub(super) async fn execute_action(
    connector: &Arc<dyn ScenarioConnector>,
    clock: &Arc<dyn ScenarioClock>,
    station: &StationDefinition,
    step: &StepDefinition,
    client: &mut Option<Box<dyn ProtocolClient>>,
    state: &mut StationState,
    selected_fault: Option<FaultKind>,
) -> Result<String, RunFailure> {
    match step.action {
        ActionKind::Connect => connect(connector, station, client, state).await,
        ActionKind::Heartbeat => client
            .as_deref()
            .ok_or_else(|| failure("not_connected", "station is not connected"))?
            .heartbeat()
            .await
            .map_err(|_| failure("heartbeat_failed", "Heartbeat exchange failed")),
        ActionKind::Boot
        | ActionKind::Authorize
        | ActionKind::Status
        | ActionKind::StartTransaction
        | ActionKind::MeterValues
        | ActionKind::StopTransaction => charging_call(step, client, state).await,
        ActionKind::AwaitRemoteStart | ActionKind::AwaitRemoteStop => {
            remote_command(step, client, state, selected_fault).await
        }
        ActionKind::TargetOffline => Ok(target_state(state, false)),
        ActionKind::TargetOnline => Ok(target_state(state, true)),
        ActionKind::ReconcileCommand => reconcile_command(step, state),
        ActionKind::Wait => {
            let duration_ms = step.duration_ms.expect("validated wait duration");
            clock.sleep(Duration::from_millis(duration_ms)).await;
            Ok(format!("{duration_ms}ms"))
        }
        ActionKind::Disconnect => disconnect(client, state).await,
    }
}

async fn connect(
    connector: &Arc<dyn ScenarioConnector>,
    station: &StationDefinition,
    client: &mut Option<Box<dyn ProtocolClient>>,
    state: &mut StationState,
) -> Result<String, RunFailure> {
    if client.is_some() {
        return Err(failure("already_connected", "station is already connected"));
    }
    let connected = connector
        .connect(station.client_config())
        .await
        .map_err(|_| {
            RunFailure::new(
                FailureCategory::Setup,
                "peer_unavailable",
                "station peer is unavailable or rejected the connection",
            )
        })?;
    let detail = connected.version().websocket_protocol().to_owned();
    *client = Some(connected);
    state.connected = true;
    Ok(detail)
}

async fn disconnect(
    client: &mut Option<Box<dyn ProtocolClient>>,
    state: &mut StationState,
) -> Result<String, RunFailure> {
    let Some(connected) = client.take() else {
        return Err(failure("not_connected", "station is not connected"));
    };
    connected
        .shutdown()
        .await
        .map_err(|_| failure("disconnect_failed", "station disconnect failed"))?;
    state.connected = false;
    Ok("stopped".to_owned())
}

async fn charging_call(
    step: &StepDefinition,
    client: &mut Option<Box<dyn ProtocolClient>>,
    state: &mut StationState,
) -> Result<String, RunFailure> {
    let action = match step.action {
        ActionKind::Boot => SimulatorAction::BootNotification,
        ActionKind::Authorize => SimulatorAction::Authorize,
        ActionKind::Status => SimulatorAction::StatusNotification,
        ActionKind::StartTransaction => SimulatorAction::StartTransaction,
        ActionKind::MeterValues => SimulatorAction::MeterValues,
        ActionKind::StopTransaction => SimulatorAction::StopTransaction,
        _ => unreachable!(),
    };
    match state.version {
        crate::OcppVersion::V1_6 => validate_before_16(state, step, action)?,
        crate::OcppVersion::V2_0_1 => super::execution_201::validate_before(state, step, action)?,
    }
    let response = client
        .as_deref()
        .ok_or_else(|| failure("not_connected", "station is not connected"))?
        .call(SimulatorCall {
            action,
            payload: step.payload.clone().expect("validated charging payload"),
        })
        .await
        .map_err(|_| failure("charging_call_failed", "OCPP charging call failed"))?;
    assert_response(step, &response)?;
    match state.version {
        crate::OcppVersion::V1_6 => apply_after_16(state, step, action, &response)?,
        crate::OcppVersion::V2_0_1 => {
            super::execution_201::apply_after(state, step, action, &response)?;
        }
    }
    Ok(response.to_string())
}

async fn remote_command(
    step: &StepDefinition,
    client: &mut Option<Box<dyn ProtocolClient>>,
    state: &mut StationState,
    selected_fault: Option<FaultKind>,
) -> Result<String, RunFailure> {
    let tracked = step.request_id.as_deref().zip(step.delivery_id.as_deref());
    if let Some((request_id, delivery_id)) = tracked {
        let expired = step
            .expires_at_ms
            .is_some_and(|expires| expires <= step.execute_at_ms.expect("validated command time"));
        if state.admit_command(request_id, delivery_id, expired) == CommandAdmission::Duplicate {
            return Ok("duplicate_suppressed".to_owned());
        }
        if expired {
            return Err(failure(
                "command_expired",
                "command expired before protocol dispatch",
            ));
        }
    }
    let command = client
        .as_deref()
        .ok_or_else(|| failure("not_connected", "station is not connected"))?
        .next_remote_command()
        .await
        .map_err(|_| failure("remote_command_failed", "remote command was not received"))?;
    let expected = if matches!(step.action, ActionKind::AwaitRemoteStart) {
        RemoteCommandKind::StartTransaction
    } else {
        RemoteCommandKind::StopTransaction
    };
    if command.kind != expected {
        return Err(failure(
            "unexpected_remote_command",
            "received a different remote command than expected",
        ));
    }
    let response = serde_json::json!({"accepted": command.accepted, "request": command.payload});
    assert_response(step, &response)?;
    if let Some((request_id, _)) = tracked {
        let response_lost = matches!(selected_fault, Some(FaultKind::MissingResponse));
        state.complete_command(request_id, command.accepted, response_lost);
        if response_lost && command.accepted {
            return Err(failure(
                "transmission_uncertain",
                "charger may have acted but its response was lost",
            ));
        }
    }
    Ok(response.to_string())
}

fn target_state(state: &mut StationState, online: bool) -> String {
    state.set_target_online(online);
    let status = if state.target_online() {
        "online"
    } else {
        "offline"
    };
    format!("{status}:physical_effects={}", state.physical_effects())
}

fn reconcile_command(
    step: &StepDefinition,
    state: &mut StationState,
) -> Result<String, RunFailure> {
    let request_id = step
        .request_id
        .as_deref()
        .expect("validated request identity");
    if !state.reconcile_command(request_id) {
        return Err(failure(
            "command_not_uncertain",
            "only a transmission-uncertain command can be reconciled",
        ));
    }
    Ok(format!(
        "confirmed_without_replay:physical_effects={}",
        state.physical_effects()
    ))
}

fn assert_response(step: &StepDefinition, actual: &serde_json::Value) -> Result<(), RunFailure> {
    if step
        .expect_response
        .as_ref()
        .is_some_and(|expected| expected != actual)
    {
        Err(failure(
            "unexpected_protocol_response",
            "protocol response did not match the expected value",
        ))
    } else {
        Ok(())
    }
}

fn validate_before_16(
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
        if !state.is_authorized(id_tag) {
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

fn apply_after_16(
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

fn failure(code: &'static str, message: &'static str) -> RunFailure {
    RunFailure::new(FailureCategory::Assertion, code, message)
}
