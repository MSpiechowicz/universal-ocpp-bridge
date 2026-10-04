use super::execution::{assert_response, failure};
use super::state::StationResource;
use super::{
    ActionKind, RunFailure, ScenarioConnector, StationDefinition, StationState, StepDefinition,
};
use crate::local_authorization::transport::NativeReplyFault;
use crate::local_authorization201::LocalAuthorization201Handle;
use crate::{ProtocolClient, SimulatorAction, SimulatorCall};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

fn initialize(station: &StationDefinition, state: &mut StationState) -> Result<(), RunFailure> {
    if state.local201.is_none() {
        let config = station.local_authorization.as_ref().ok_or_else(|| {
            failure(
                "native_state_unavailable",
                "native private state is not configured",
            )
        })?;
        state.local201 = Some(
            LocalAuthorization201Handle::open(&station.id, config)
                .map_err(|code| failure(code, "native private state could not be opened"))?,
        );
    }
    Ok(())
}
pub(super) async fn execute(
    connector: &Arc<dyn ScenarioConnector>,
    station: &StationDefinition,
    step: &StepDefinition,
    client: &mut Option<Box<dyn ProtocolClient>>,
    state: &mut StationState,
) -> Result<String, RunFailure> {
    initialize(station, state)?;
    let local = state.local201.clone().expect("initialized native state");
    match step.action {
        ActionKind::CsmsOffline => {
            let connected = client
                .take()
                .ok_or_else(|| failure("not_connected", "station is not connected"))?;
            connected
                .shutdown()
                .await
                .map_err(|_| failure("disconnect_failed", "native socket disconnect failed"))?;
            state.connected = false;
            state.registered = false;
            Ok("socket_closed".to_owned())
        }
        ActionKind::CsmsReconnect => reconnect(connector, station, client, state, local).await,
        ActionKind::OfflineStart | ActionKind::OfflineStop => {
            offline(station, step, client.is_some(), state, &local)
        }
        ActionKind::AssertLocalAuthorization | ActionKind::AwaitLocalAuthorization => {
            observe(step, &local).await
        }
        ActionKind::DelayLocalReply | ActionKind::DropLocalReply => {
            let connected = client
                .as_deref()
                .ok_or_else(|| failure("not_connected", "native fault requires a client"))?;
            let fault = if matches!(step.action, ActionKind::DelayLocalReply) {
                NativeReplyFault::Delay(Duration::from_millis(
                    step.duration_ms.expect("validated delay"),
                ))
            } else {
                NativeReplyFault::DropConnection
            };
            connected.arm_local_reply_fault(fault).map_err(|_| {
                failure("native_fault_unavailable", "native reply fault unavailable")
            })?;
            Ok("armed".to_owned())
        }
        ActionKind::AwaitReboot => {
            let connected = client
                .as_deref()
                .ok_or_else(|| failure("not_connected", "reboot observation requires a client"))?;
            loop {
                if connected.reboot_count() > state.observed_reboots {
                    state.observed_reboots = connected.reboot_count();
                    state.registered = connected.accepted_registration().unwrap_or(false);
                    return Ok("disk_recovered".to_owned());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        _ => unreachable!(),
    }
}
pub(super) async fn await_boot_replay(
    state: &StationState,
    client: Option<&dyn ProtocolClient>,
    action: SimulatorAction,
    response: &Value,
) -> Result<(), RunFailure> {
    if action != SimulatorAction::BootNotification || response["status"] != "Accepted" {
        return Ok(());
    }
    if let Some(local) = &state.local201 {
        let connected =
            client.ok_or_else(|| failure("not_connected", "native replay requires a client"))?;
        local
            .replay(connected)
            .await
            .map_err(|code| failure(code, "original offline delivery unavailable"))?;
    }
    Ok(())
}

async fn reconnect(
    connector: &Arc<dyn ScenarioConnector>,
    station: &StationDefinition,
    client: &mut Option<Box<dyn ProtocolClient>>,
    state: &mut StationState,
    local: LocalAuthorization201Handle,
) -> Result<String, RunFailure> {
    if client
        .as_deref()
        .is_some_and(|client| client.socket_connected() != Some(false))
    {
        return Err(failure("already_connected", "station is already connected"));
    }
    if let Some(previous) = client.take() {
        let _ = previous.force_shutdown().await;
    }
    // Release the exclusive old owner before opening its recovered file.
    state.local201 = None;
    drop(local);
    let connected = connector
        .connect(station.client_config())
        .await
        .map_err(|_| failure("peer_unavailable", "native socket reconnect failed"))?;
    state.local201 = connected.local_authorization201();
    *client = Some(connected);
    state.connected = true;
    let boot = state.boot_payload.clone().unwrap_or_else(|| json!({
        "reason":"PowerUp","chargingStation":{"model":"Local Authorization","vendorName":"UOB Simulator"}
    }));
    let connected = client.as_deref().expect("connected native client");
    let response = connected
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: boot,
        })
        .await
        .map_err(|_| failure("boot_failed", "native reconnect Boot failed"))?;
    state.registered = response["status"] == "Accepted";
    if !state.registered {
        return Ok("registration_denied_no_replay".to_owned());
    }
    await_boot_replay(
        state,
        Some(connected),
        SimulatorAction::BootNotification,
        &response,
    )
    .await?;
    Ok("registered_and_replayed".to_owned())
}

fn offline(
    station: &StationDefinition,
    step: &StepDefinition,
    socket_open: bool,
    state: &mut StationState,
    local: &LocalAuthorization201Handle,
) -> Result<String, RunFailure> {
    if socket_open {
        return Err(failure(
            "csms_socket_open",
            "offline operation requires a closed socket",
        ));
    }
    let payload = step.payload.as_ref().expect("validated offline payload");
    let evse = payload
        .pointer("/evse/id")
        .and_then(Value::as_u64)
        .and_then(|n| u16::try_from(n).ok());
    let connector_id = payload
        .pointer("/evse/connectorId")
        .and_then(Value::as_u64)
        .and_then(|n| u16::try_from(n).ok());
    if !evse
        .zip(connector_id)
        .is_some_and(|resource| station.evse_connectors().contains(&resource))
    {
        return Err(failure(
            "unknown_evse",
            "offline native resource is not configured",
        ));
    }
    if matches!(step.action, ActionKind::OfflineStart)
        && station
            .evse_connectors()
            .iter()
            .any(|&(evse_id, connector_id)| {
                Some(evse_id) == evse
                    && state
                        .resource(StationResource::EvseConnector {
                            evse_id,
                            connector_id,
                        })
                        .is_some_and(|resource| resource.transaction_id.is_some())
            })
    {
        return Err(failure(
            "offline_transaction_active",
            "native EVSE already has an online transaction",
        ));
    }
    let payload = payload.clone();
    let outcome = if matches!(step.action, ActionKind::OfflineStart) {
        let accepted = local
            .offline_start(payload)
            .map_err(|code| failure(code, "native offline start failed"))?;
        if accepted {
            state.record_local_effect();
        }
        json!({"accepted":accepted})
    } else {
        local
            .offline_stop(payload)
            .map_err(|code| failure(code, "native offline stop failed"))?;
        json!({"stopped":true})
    };
    assert_response(step, &outcome)?;
    Ok(outcome.to_string())
}

async fn observe(
    step: &StepDefinition,
    local: &LocalAuthorization201Handle,
) -> Result<String, RunFailure> {
    let expected = step
        .expect_response
        .as_ref()
        .ok_or_else(|| failure("missing_native_assertion", "safe counters are required"))?;
    if !expected.as_object().is_some_and(|fields| {
        !fields.is_empty()
            && fields.iter().all(|(key, value)| {
                (key == "stateAvailable" && value.is_boolean())
                    || (matches!(
                        key.as_str(),
                        "listVersion"
                            | "listEntries"
                            | "cacheEntries"
                            | "offlineRecords"
                            | "uncertainRecords"
                    ) && value.as_u64().is_some())
            })
    }) {
        return Err(failure(
            "invalid_native_assertion",
            "native assertions accept only safe counters",
        ));
    }
    loop {
        let snapshot = if matches!(step.action, ActionKind::AwaitLocalAuthorization) {
            local.settled_snapshot()
        } else {
            Some(local.snapshot())
        };
        if let Some(snapshot) = snapshot.filter(|actual| {
            expected
                .as_object()
                .expect("validated assertions")
                .iter()
                .all(|(key, value)| actual.get(key) == Some(value))
        }) {
            return Ok(snapshot.to_string());
        }
        if matches!(step.action, ActionKind::AssertLocalAuthorization) {
            return Err(failure(
                "unexpected_native_state",
                "native counters did not match",
            ));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
