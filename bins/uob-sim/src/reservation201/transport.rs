//! Native wire boundary: validate the exact OCA request, answer from the actual model, then
//! report reservation views and status updates only through correlated outbound CALLs.
use super::{ConnectorStatus, Reservation201Handle, ReserveNowRequest, StatusUpdate, UpdateStatus};
use crate::local_authorization201::{IdToken, IdTokenInfo, validation};
use crate::{
    Ocpp201State, SimulatorAction, SimulatorCall, SimulatorClientError, TraceBuffer, TraceKind,
};
use ocpp_client::ocpp_2_0_1::OCPP2_0_1Client;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use tokio::task::JoinSet;

static RESERVE_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ReserveNowRequest.json"
    ))
    .expect("pinned schema")
});
static CANCEL_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/CancelReservationRequest.json"
    ))
    .expect("pinned schema")
});
const RETRY: Duration = Duration::from_secs(1);

/// Held while an outbound station CALL is prepared and exchanged, so derived reports never
/// overtake the station's own message that caused them.
pub(crate) struct OutboundHold(Arc<Mutex<Ocpp201State>>);
impl OutboundHold {
    pub(crate) fn new(state: &Arc<Mutex<Ocpp201State>>) -> Self {
        state.lock().expect("native state lock").reservation_holds += 1;
        Self(Arc::clone(state))
    }
}
impl Drop for OutboundHold {
    fn drop(&mut self) {
        self.0.lock().expect("native state lock").reservation_holds -= 1;
    }
}

fn handle(state: &Arc<Mutex<Ocpp201State>>) -> Option<Reservation201Handle> {
    state
        .lock()
        .expect("native state lock")
        .reservation201
        .clone()
}

/// Inbound `ReserveNow`/`CancelReservation`; schema violations never reach the model.
pub(crate) fn reply(action: &str, payload: &Value, state: &Arc<Mutex<Ocpp201State>>) -> Value {
    let now = OffsetDateTime::now_utc();
    let invalid = json!({"callError":"FormationViolation"});
    match action {
        "ReserveNow" => {
            if !validation::valid_native(&RESERVE_SCHEMA, payload)
                || !validation::valid_token(&payload["idToken"])
                || payload
                    .get("groupIdToken")
                    .is_some_and(|group| !validation::valid_token(group))
            {
                return invalid;
            }
            let Ok(request) = ReserveNowRequest::deserialize(payload) else {
                return invalid;
            };
            let Some(handle) = handle(state) else {
                return json!({"status":"Rejected"});
            };
            handle.reserve(request, now).map_or_else(
                |_| json!({"callError":"InternalError"}),
                |status| json!({"status":status}),
            )
        }
        "CancelReservation" => {
            let Some(id) = payload["reservationId"]
                .as_i64()
                .and_then(|id| i32::try_from(id).ok())
                .filter(|_| validation::valid_native(&CANCEL_SCHEMA, payload))
            else {
                return invalid;
            };
            let Some(handle) = handle(state) else {
                return json!({"status":"Rejected"});
            };
            handle.cancel(id, now).map_or_else(
                |_| json!({"callError":"InternalError"}),
                |accepted| json!({"status":if accepted {"Accepted"} else {"Rejected"}}),
            )
        }
        _ => unreachable!("only reservation actions are routed here"),
    }
}

fn unsigned(payload: &Value, pointer: &str) -> Option<u16> {
    payload
        .pointer(pointer)
        .and_then(Value::as_u64)
        .and_then(|id| u16::try_from(id).ok())
}

/// Apply actual station facts before their outbound CALL; a refused start is never sent.
pub(crate) async fn prepare(
    client: &OCPP2_0_1Client,
    state: &Arc<Mutex<Ocpp201State>>,
    call: &mut SimulatorCall,
    generation: u64,
) -> Result<(), SimulatorClientError> {
    let (reservation, local) = {
        let current = state.lock().expect("native state lock");
        (current.reservation201.clone(), current.local.clone())
    };
    let Some(reservation) = reservation else {
        return Ok(());
    };
    let error = |code: &'static str| SimulatorClientError::Protocol(code.to_owned());
    match call.action {
        SimulatorAction::StatusNotification => {
            let evse = unsigned(&call.payload, "/evseId")
                .ok_or_else(|| error("reservation_status_invalid"))?;
            let connector = unsigned(&call.payload, "/connectorId")
                .ok_or_else(|| error("reservation_status_invalid"))?;
            let status = match call.payload["connectorStatus"].as_str() {
                Some("Available") => ConnectorStatus::Available,
                Some("Occupied") => ConnectorStatus::Occupied,
                Some("Unavailable") => ConnectorStatus::Unavailable,
                Some("Faulted") => ConnectorStatus::Faulted,
                Some("Reserved") => return Ok(()),
                _ => return Err(error("reservation_status_invalid")),
            };
            reservation
                .set_connector(evse, connector, status)
                .map_err(error)?;
        }
        SimulatorAction::StartTransaction if call.payload["eventType"] == "Started" => {
            let evse = unsigned(&call.payload, "/evse/id")
                .ok_or_else(|| error("reservation_evse_invalid"))?;
            let connector = unsigned(&call.payload, "/evse/connectorId")
                .ok_or_else(|| error("reservation_evse_invalid"))?;
            let expected = call
                .payload
                .get("reservationId")
                .map(|id| {
                    id.as_i64()
                        .and_then(|id| i32::try_from(id).ok())
                        .ok_or_else(|| error("reservation_start_id_invalid"))
                })
                .transpose()?;
            let now = OffsetDateTime::now_utc();
            let matched = if let Some(token) = call.payload.get("idToken") {
                if !validation::valid_token(token) {
                    return Err(error("reservation_identity_invalid"));
                }
                let token = IdToken::deserialize(token)
                    .map_err(|_| error("reservation_identity_invalid"))?;
                let info = match local.and_then(|local| local.identity_info(&token, now)) {
                    Some(info) => info,
                    None => Box::pin(authorize(client, &token, generation)).await?,
                };
                let current = state.lock().expect("native state lock");
                if current.socket_generation != generation
                    || !current.socket_connected
                    || !current.registered
                {
                    return Err(error("reservation_start_generation_unavailable"));
                }
                drop(current);
                let authorized = crate::local_authorization201::allows(&info, i32::from(evse), now);
                reservation
                    .start_expected(
                        evse,
                        connector,
                        Some((&token, info.group_id_token.as_ref())),
                        authorized,
                        now,
                        expected,
                    )
                    .map_err(error)?
            } else {
                reservation
                    .start_expected(evse, connector, None, false, now, expected)
                    .map_err(error)?
            };
            if let Some(id) = matched {
                call.payload["reservationId"] = id.into();
            }
        }
        _ => {}
    }
    Ok(())
}

/// H03.FR.08: an identity absent from the list and cache is resolved by the CSMS.
async fn authorize(
    client: &OCPP2_0_1Client,
    token: &IdToken,
    generation: u64,
) -> Result<IdTokenInfo, SimulatorClientError> {
    let error =
        || SimulatorClientError::Protocol("reservation_authorization_unavailable".to_owned());
    let request = serde_json::from_value(json!({"idToken":token})).map_err(|_| error())?;
    let response = Box::pin(crate::local_authorization201::transport::on_generation(
        generation,
        None,
        client.send_authorize(request),
    ))
    .await
    .map_err(|_| error())?;
    let mut response = serde_json::to_value(response).map_err(|_| error())?;
    let info = if validation::valid_info(&response["idTokenInfo"]) {
        IdTokenInfo::deserialize(&response["idTokenInfo"]).map_err(|_| error())
    } else {
        Err(error())
    };
    crate::local_authorization201::wipe_json(&mut response);
    info
}

/// Expiry always runs; reports drain in causal order once registered and no reply is pending.
pub(crate) fn tick(
    client: &OCPP2_0_1Client,
    state: &Arc<Mutex<Ocpp201State>>,
    tasks: &mut JoinSet<()>,
    traces: &TraceBuffer,
) {
    let (reservation, ready, generation) = {
        let current = state.lock().expect("native state lock");
        let idle = current
            .local
            .as_ref()
            .is_none_or(crate::local_authorization201::LocalAuthorization201Handle::reply_idle);
        (
            current.reservation201.clone(),
            idle && current.reservation_holds == 0
                && current.registered
                && current.socket_connected
                && current
                    .reservation_retry_at
                    .is_none_or(|at| Instant::now() >= at),
            current.socket_generation,
        )
    };
    let Some(reservation) = reservation else {
        return;
    };
    if reservation.expire(OffsetDateTime::now_utc()).is_err() {
        traces.push(TraceKind::Failed, "reservation_expiry_uncertain");
    }
    if !ready || !tasks.is_empty() {
        return;
    }
    let statuses = reservation.take_statuses();
    let updates = reservation.updates();
    if statuses.is_empty() && updates.is_empty() {
        return;
    }
    let client = client.clone();
    let state = Arc::clone(state);
    let traces = traces.clone();
    tasks.spawn(async move {
        let failed = |state: &Arc<Mutex<Ocpp201State>>| {
            state
                .lock()
                .expect("native state lock")
                .reservation_retry_at = Some(Instant::now() + RETRY);
            traces.push(TraceKind::Failed, "reservation_report_uncertain");
        };
        for (index, ((evse, connector), status)) in statuses.iter().enumerate() {
            if notify(&client, &state, generation, (*evse, *connector), status)
                .await
                .is_err()
            {
                reservation.requeue_statuses(statuses[index..].to_vec());
                failed(&state);
                return;
            }
        }
        for update in updates {
            if report(&client, generation, update).await.is_err()
                || reservation.acknowledge(update).is_err()
            {
                failed(&state);
                return;
            }
        }
        state
            .lock()
            .expect("native state lock")
            .reservation_retry_at = None;
    });
}

async fn notify(
    client: &OCPP2_0_1Client,
    state: &Arc<Mutex<Ocpp201State>>,
    generation: u64,
    (evse, connector): (u16, u16),
    status: &str,
) -> Result<(), SimulatorClientError> {
    let payload = json!({"timestamp":crate::trigger201::now(),"connectorStatus":status,"evseId":evse,"connectorId":connector});
    let request = serde_json::from_value(payload.clone())
        .map_err(|_| SimulatorClientError::Protocol("reservation_status_invalid".to_owned()))?;
    crate::local_authorization201::transport::on_generation(
        generation,
        None,
        client.send_status_notification(request),
    )
    .await
    .map_err(|_| SimulatorClientError::Protocol("reservation_status_uncertain".to_owned()))?;
    let mut current = state.lock().expect("native state lock");
    if current.socket_generation == generation {
        current.status.insert((evse, connector), payload);
    }
    Ok(())
}

async fn report(
    client: &OCPP2_0_1Client,
    generation: u64,
    update: StatusUpdate,
) -> Result<(), SimulatorClientError> {
    let status = match update.reservation_update_status {
        UpdateStatus::Expired => "Expired",
        UpdateStatus::Removed => "Removed",
    };
    let request = serde_json::from_value(
        json!({"reservationId":update.reservation_id,"reservationUpdateStatus":status}),
    )
    .map_err(|_| SimulatorClientError::Protocol("reservation_update_invalid".to_owned()))?;
    crate::local_authorization201::transport::on_generation(
        generation,
        None,
        client.send_reservation_status_update(request),
    )
    .await
    .map(|_| ())
    .map_err(|_| SimulatorClientError::Protocol("reservation_update_uncertain".to_owned()))
}
