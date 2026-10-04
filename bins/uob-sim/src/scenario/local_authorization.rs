use std::sync::Arc;
use std::time::Duration;

use super::execution::{assert_response, failure};
use super::{
    ActionKind, RunFailure, ScenarioConnector, StationDefinition, StationResource, StationState,
    StepDefinition,
};
use crate::local_authorization::{LocalAuthorizationHandle, transport::NativeReplyFault};
use crate::{OcppVersion, ProtocolClient, SimulatorAction, SimulatorCall};

pub(super) fn initialize(
    station: &StationDefinition,
    state: &mut StationState,
) -> Result<(), RunFailure> {
    if state.local.is_none()
        && let Some(config) = &station.local_authorization
    {
        state.local = Some(
            LocalAuthorizationHandle::open(&station.id, config)
                .map_err(|code| failure(code, "private native state could not be opened"))?,
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
    if state.version != OcppVersion::V1_6 {
        return Err(failure(
            "wrong_local_authorization_protocol",
            "native local authorization requires OCPP 1.6",
        ));
    }
    initialize(station, state)?;
    let local = state.local.clone().ok_or_else(|| {
        failure(
            "native_state_unavailable",
            "native local authorization state is unavailable",
        )
    })?;
    match step.action {
        ActionKind::CsmsOffline => {
            let connected = client
                .take()
                .ok_or_else(|| failure("not_connected", "station is not connected"))?;
            connected
                .shutdown()
                .await
                .map_err(|_| failure("disconnect_failed", "CSMS socket disconnect failed"))?;
            state.connected = false;
            state.registered = false;
            Ok("socket_closed".to_owned())
        }
        ActionKind::CsmsReconnect => reconnect(connector, station, client, state, &local).await,
        ActionKind::OfflineStart => offline_start(station, step, client.as_deref(), state, &local),
        ActionKind::OfflineStop => {
            require_offline(client.as_deref())?;
            let mut payload = step
                .payload
                .clone()
                .expect("validated offline stop payload");
            let connector = payload
                .as_object_mut()
                .and_then(|fields| fields.remove("connectorId"))
                .and_then(|id| id.as_u64())
                .and_then(|id| u16::try_from(id).ok())
                .ok_or_else(|| failure("invalid_connector", "offline stop requires connectorId"))?;
            local
                .offline_stop(connector, payload)
                .map_err(|code| failure(code, "native offline stop failed"))?;
            let outcome = serde_json::json!({"stopped":true});
            assert_response(step, &outcome)?;
            Ok(outcome.to_string())
        }
        ActionKind::AssertLocalAuthorization | ActionKind::AwaitLocalAuthorization => {
            observe_state(step, &local).await
        }
        ActionKind::AwaitReboot => observe_reboot(client.as_deref(), state, &local).await,
        ActionKind::DelayLocalReply | ActionKind::DropLocalReply => {
            let connected = client
                .as_deref()
                .ok_or_else(|| failure("not_connected", "native reply fault requires a client"))?;
            let fault = if matches!(step.action, ActionKind::DelayLocalReply) {
                NativeReplyFault::Delay(Duration::from_millis(
                    step.duration_ms.expect("validated native delay"),
                ))
            } else {
                NativeReplyFault::DropConnection
            };
            connected.arm_local_reply_fault(fault).map_err(|_| {
                failure(
                    "native_fault_unavailable",
                    "native reply fault could not be armed",
                )
            })?;
            Ok("armed".to_owned())
        }
        _ => unreachable!(),
    }
}

fn require_offline(client: Option<&dyn ProtocolClient>) -> Result<(), RunFailure> {
    if client.is_some() {
        Err(failure(
            "csms_socket_open",
            "offline operation requires a closed CSMS socket",
        ))
    } else {
        Ok(())
    }
}

fn validate_expected(expected: &serde_json::Value) -> Result<(), RunFailure> {
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
                    ) && value.as_i64().is_some())
            })
    }) {
        return Err(failure(
            "invalid_native_assertion",
            "native state assertions accept only safe aggregate fields",
        ));
    }
    Ok(())
}

fn matches_expected(expected: &serde_json::Value, actual: &serde_json::Value) -> bool {
    expected
        .as_object()
        .expect("validated native assertion")
        .iter()
        .all(|(key, value)| actual.get(key) == Some(value))
}

pub(super) fn safe_response(response: &serde_json::Value) -> serde_json::Value {
    let mut safe = serde_json::Map::new();
    for key in [
        "status",
        "transactionId",
        "currentTime",
        "interval",
        "accepted",
    ] {
        if let Some(value) = response.get(key) {
            safe.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(status) = response.pointer("/idTagInfo/status") {
        safe.insert("idTagInfo".to_owned(), serde_json::json!({"status":status}));
    }
    serde_json::Value::Object(safe)
}

async fn reconnect(
    connector: &Arc<dyn ScenarioConnector>,
    station: &StationDefinition,
    client: &mut Option<Box<dyn ProtocolClient>>,
    state: &mut StationState,
    local: &LocalAuthorizationHandle,
) -> Result<String, RunFailure> {
    if client
        .as_deref()
        .is_some_and(|existing| existing.socket_connected() != Some(false))
    {
        return Err(failure("already_connected", "station is already connected"));
    }
    if let Some(closed) = client.take() {
        closed
            .shutdown()
            .await
            .map_err(|_| failure("disconnect_failed", "closed CSMS client teardown failed"))?;
        state.connected = false;
        state.registered = false;
    }
    let mut config = station.client_config();
    config.local_authorization = Some(local.clone());
    let connected = connector
        .connect(config)
        .await
        .map_err(|_| failure("peer_unavailable", "CSMS socket reconnect failed"))?;
    *client = Some(connected);
    state.connected = true;
    let connected = client.as_deref().expect("connected native client");
    state.observed_reboots = connected.reboot_count();
    let boot = state.boot_payload.clone().unwrap_or_else(|| {
        serde_json::json!({
            "chargePointVendor":"UOB Simulator", "chargePointModel":"Local Authorization"
        })
    });
    let response = connected
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: boot,
        })
        .await
        .map_err(|_| failure("boot_failed", "native reconnect BootNotification failed"))?;
    if response.get("status").and_then(serde_json::Value::as_str) != Some("Accepted") {
        return Err(failure(
            "station_not_registered",
            "native reconnect registration denied",
        ));
    }
    state.registered = true;
    let replay_failed = local.replay(connected).await.is_err();
    let snapshot = local.snapshot();
    if snapshot["stateAvailable"] != true {
        return Err(failure(
            "private_state_unavailable",
            "native private state is unavailable",
        ));
    }
    if replay_failed
        || snapshot["uncertainRecords"]
            .as_u64()
            .expect("native uncertainty counter")
            > 0
    {
        Ok("registered_replay_uncertain".to_owned())
    } else {
        Ok("registered_and_replayed".to_owned())
    }
}

fn offline_start(
    station: &StationDefinition,
    step: &StepDefinition,
    client: Option<&dyn ProtocolClient>,
    state: &mut StationState,
    local: &LocalAuthorizationHandle,
) -> Result<String, RunFailure> {
    require_offline(client)?;
    let payload = step
        .payload
        .clone()
        .expect("validated offline start payload");
    let connector_id = payload
        .get("connectorId")
        .and_then(serde_json::Value::as_u64)
        .and_then(|id| u16::try_from(id).ok())
        .ok_or_else(|| failure("invalid_connector", "offline start requires connectorId"))?;
    if !station.connector_ids().contains(&connector_id) {
        return Err(failure(
            "unknown_connector",
            "offline start connector is not configured",
        ));
    }
    if state
        .resource(StationResource::Connector { connector_id })
        .is_some_and(|resource| resource.transaction_id.is_some())
    {
        return Err(failure(
            "offline_transaction_active",
            "connector already has an active transaction",
        ));
    }
    let accepted = local
        .offline_start(payload)
        .map_err(|code| failure(code, "native offline start failed"))?;
    let outcome = serde_json::json!({"accepted":accepted});
    assert_response(step, &outcome)?;
    if accepted {
        state.record_local_effect();
    }
    Ok(outcome.to_string())
}

async fn observe_state(
    step: &StepDefinition,
    local: &LocalAuthorizationHandle,
) -> Result<String, RunFailure> {
    let expected = step.expect_response.as_ref().ok_or_else(|| {
        failure(
            "missing_native_assertion",
            "native state assertion requires expect_response",
        )
    })?;
    validate_expected(expected)?;
    loop {
        let snapshot = if matches!(step.action, ActionKind::AssertLocalAuthorization) {
            Some(local.snapshot())
        } else {
            // Do not let the following real disconnect race a committed
            // native mutation whose reply has not yet reached the wire.
            local.settled_snapshot()
        };
        if let Some(snapshot) = snapshot.filter(|snapshot| matches_expected(expected, snapshot)) {
            return Ok(snapshot.to_string());
        }
        if matches!(step.action, ActionKind::AssertLocalAuthorization) {
            return Err(failure(
                "unexpected_native_state",
                "native local authorization state did not match",
            ));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn observe_reboot(
    client: Option<&dyn ProtocolClient>,
    state: &mut StationState,
    local: &LocalAuthorizationHandle,
) -> Result<String, RunFailure> {
    if !local.has_persistence() {
        return Err(failure(
            "persistent_native_state_required",
            "disk recovery observation requires private persistent native state",
        ));
    }
    let connected =
        client.ok_or_else(|| failure("not_connected", "reboot observation requires a client"))?;
    loop {
        let count = connected.reboot_count();
        if count > state.observed_reboots {
            state.observed_reboots = count;
            state.registered = connected.accepted_registration().unwrap_or(false);
            return Ok("disk_recovered_and_registered".to_owned());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub(super) async fn await_boot_replay(
    state: &StationState,
    client: Option<&dyn ProtocolClient>,
    action: SimulatorAction,
    response: &serde_json::Value,
) -> Result<(), RunFailure> {
    if state.version != OcppVersion::V1_6
        || action != SimulatorAction::BootNotification
        || response.get("status").and_then(serde_json::Value::as_str) != Some("Accepted")
    {
        return Ok(());
    }
    let Some(local) = &state.local else {
        return Ok(());
    };
    let connected =
        client.ok_or_else(|| failure("not_connected", "native replay requires a client"))?;
    let replay_failed = local.replay(connected).await.is_err();
    let snapshot = local.snapshot();
    if snapshot["stateAvailable"] != true {
        return Err(failure(
            "private_state_unavailable",
            "native private state is unavailable",
        ));
    }
    if replay_failed
        || snapshot["uncertainRecords"]
            .as_u64()
            .expect("native uncertainty counter")
            > 0
    {
        return Err(failure(
            "offline_replay_uncertain",
            "native Boot accepted but offline replay remains uncertain",
        ));
    }
    Ok(())
}
