use std::sync::Arc;
use std::time::Duration;

use super::{
    ActionKind, CommandAdmission, DiagnosticCounts, FailureCategory, FaultKind, LiveRun,
    RunFailure, ScenarioClock, ScenarioConnector, StationDefinition, StationState, StepDefinition,
};
use crate::{
    ClientDiagnostics, ProtocolClient, RemoteCommandKind, ReplyDelayReceipt, SimulatorAction,
    SimulatorCall,
};
use tokio::time::timeout;

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_action(
    connector: &Arc<dyn ScenarioConnector>,
    clock: &Arc<dyn ScenarioClock>,
    station: &StationDefinition,
    step: &StepDefinition,
    client: &mut Option<Box<dyn ProtocolClient>>,
    state: &mut StationState,
    selected_fault: Option<FaultKind>,
    delayed_reply: Option<ReplyDelayReceipt>,
    live: &LiveRun,
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
            remote_command(step, client, state, selected_fault, delayed_reply, live).await
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
        ActionKind::AssertReservation | ActionKind::AwaitReservation => {
            super::reservation16::observe(step, client.as_deref()).await
        }
        ActionKind::AssertFirmware | ActionKind::AwaitFirmware => {
            super::firmware16::observe(step, client.as_deref()).await
        }
        ActionKind::CsmsOffline
        | ActionKind::CsmsReconnect
        | ActionKind::OfflineStart
        | ActionKind::OfflineStop
        | ActionKind::AssertLocalAuthorization
        | ActionKind::AwaitLocalAuthorization
        | ActionKind::AwaitReboot
        | ActionKind::DelayLocalReply
        | ActionKind::DropLocalReply => {
            super::local_authorization::execute(connector, station, step, client, state).await
        }
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
    super::local_authorization::initialize(station, state)?;
    let mut configuration = station.client_config();
    configuration.local_authorization.clone_from(&state.local);
    state.local201 = None;
    let connected = connector.connect(configuration).await.map_err(|_| {
        RunFailure::new(
            FailureCategory::Setup,
            "peer_unavailable",
            "station peer is unavailable or rejected the connection",
        )
    })?;
    let detail = connected.version().websocket_protocol().to_owned();
    *client = Some(connected);
    if state.local.is_none() {
        state.local = client
            .as_deref()
            .and_then(ProtocolClient::local_authorization)
            // Only a durable owner may be carried into another native connection.
            .filter(crate::local_authorization::LocalAuthorizationHandle::has_persistence);
    }
    state.local201 = client
        .as_deref()
        .and_then(ProtocolClient::local_authorization201)
        .filter(crate::local_authorization201::LocalAuthorization201Handle::has_persistence);
    state.awaited_remote_start_id = None;
    state.connected = true;
    state.observed_reboots = client.as_deref().map_or(0, ProtocolClient::reboot_count);
    Ok(detail)
}

async fn disconnect(
    client: &mut Option<Box<dyn ProtocolClient>>,
    state: &mut StationState,
) -> Result<String, RunFailure> {
    let Some(connected) = client.take() else {
        return Err(failure("not_connected", "station is not connected"));
    };
    state.awaited_remote_start_id = None;
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
    if let Some(registered) = client
        .as_deref()
        .and_then(ProtocolClient::accepted_registration)
    {
        state.registered = registered;
    }
    let bound;
    let step = if step.use_active_transaction {
        // Bind before validation so the stop is checked against the actual open transaction.
        let mut value = step.clone();
        let id = state
            .single_active_transaction()
            .and_then(|id| id.parse::<i64>().ok())
            .ok_or_else(|| {
                failure(
                    "transaction_not_active",
                    "exactly one simulator transaction must be active",
                )
            })?;
        value.payload.as_mut().expect("validated charging payload")["transactionId"] = id.into();
        bound = value;
        &bound
    } else {
        step
    };
    match state.version {
        crate::OcppVersion::V1_6 => super::execution_16::validate_before(state, step, action)?,
        crate::OcppVersion::V2_0_1 => super::execution_201::validate_before(state, step, action)?,
    }
    let mut payload = step.payload.clone().expect("validated charging payload");
    if step.use_current_timestamp {
        payload["timestamp"] = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|_| {
                failure(
                    "source_clock_unavailable",
                    "native source timestamp could not be generated",
                )
            })?
            .into();
    }
    if step.use_awaited_remote_start_id {
        if state.version != crate::OcppVersion::V2_0_1 {
            return Err(failure(
                "invalid_remote_start_binding",
                "remote start ID binding requires OCPP 2.0.1",
            ));
        }
        let remote_start_id = state.awaited_remote_start_id.take().ok_or_else(|| {
            failure(
                "remote_start_id_unavailable",
                "no accepted remote start ID is available on this connection",
            )
        })?;
        payload["transactionInfo"]["remoteStartId"] = remote_start_id.into();
    } else if matches!(step.action, ActionKind::StartTransaction) {
        state.awaited_remote_start_id = None;
    }
    let response = client
        .as_deref()
        .ok_or_else(|| failure("not_connected", "station is not connected"))?
        .call(SimulatorCall { action, payload })
        .await
        .map_err(|_| failure("charging_call_failed", "OCPP charging call failed"))?;
    assert_response(step, &response)?;
    super::local_authorization::await_boot_replay(state, client.as_deref(), action, &response)
        .await?;
    match state.version {
        crate::OcppVersion::V1_6 => {
            super::execution_16::apply_after(state, step, action, &response)?;
        }
        crate::OcppVersion::V2_0_1 => {
            super::execution_201::apply_after(state, step, action, &response)?;
        }
    }
    if action == SimulatorAction::BootNotification {
        state.boot_payload.clone_from(&step.payload);
    }
    Ok(super::local_authorization::safe_response(&response).to_string())
}

async fn remote_command(
    step: &StepDefinition,
    client: &mut Option<Box<dyn ProtocolClient>>,
    state: &mut StationState,
    selected_fault: Option<FaultKind>,
    delayed_reply: Option<ReplyDelayReceipt>,
    live: &LiveRun,
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
    if matches!(step.action, ActionKind::AwaitRemoteStart) {
        state.awaited_remote_start_id = None;
    }
    if let Some(receipt) = delayed_reply {
        receipt.await.map_err(|_| {
            failure(
                "remote_reply_delay_failed",
                "remote command reply was not delayed on the socket",
            )
        })?;
        live.applied(&step.id);
    }
    let response = serde_json::json!({"accepted": command.accepted, "request": command.payload});
    assert_response(step, &response)?;
    if let Some((request_id, _)) = tracked {
        let response_lost = matches!(selected_fault, Some(FaultKind::MissingResponse));
        state.complete_command(request_id, command.accepted, response_lost);
        if response_lost {
            live.applied(&step.id);
        }
        if response_lost && command.accepted {
            return Err(failure(
                "transmission_uncertain",
                "charger may have acted but its response was lost",
            ));
        }
    }
    if matches!(step.action, ActionKind::AwaitRemoteStart)
        && command.accepted
        && !matches!(selected_fault, Some(FaultKind::MissingResponse))
    {
        state.awaited_remote_start_id = command
            .payload
            .get("remoteStartId")
            .and_then(serde_json::Value::as_u64)
            .filter(|id| *id > 0);
        if state.version == crate::OcppVersion::V1_6 {
            let id_tag = command
                .payload
                .get("idTag")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    failure(
                        "missing_id_tag",
                        "accepted remote start did not contain an idTag",
                    )
                })?;
            state.authorize(id_tag);
        }
    }
    Ok(match state.version {
        crate::OcppVersion::V1_6 => serde_json::json!({"accepted":command.accepted}).to_string(),
        crate::OcppVersion::V2_0_1 => response.to_string(),
    })
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

pub(super) fn assert_response(
    step: &StepDefinition,
    actual: &serde_json::Value,
) -> Result<(), RunFailure> {
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

pub(super) fn failure(code: &'static str, message: &'static str) -> RunFailure {
    RunFailure::new(FailureCategory::Assertion, code, message)
}

pub(super) async fn cleanup_client(
    client: &mut Option<Box<dyn ProtocolClient>>,
    force: bool,
    diagnostics: &mut DiagnosticCounts,
) -> bool {
    let Some(client) = client.take() else {
        return false;
    };
    merge_client_diagnostics(diagnostics, client.diagnostics());
    let graceful = !force
        && matches!(
            timeout(Duration::from_secs(2), client.shutdown()).await,
            Ok(Ok(()))
        );
    let stopped = if graceful {
        true
    } else {
        let stopped = matches!(
            timeout(Duration::from_secs(2), client.force_shutdown()).await,
            Ok(Ok(()))
        );
        client.abort();
        stopped
    };
    merge_client_diagnostics(diagnostics, client.diagnostics());
    tokio::task::yield_now().await;
    stopped
}

fn merge_client_diagnostics(counts: &mut DiagnosticCounts, client: ClientDiagnostics) {
    counts.rejected_commands = counts.rejected_commands.max(client.rejected_commands);
    counts.dropped_traces = counts.dropped_traces.max(client.dropped_traces);
}
