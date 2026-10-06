use super::{ConnectorState, ReserveRequest, ReserveStatus};
use crate::{Command, Ocpp16State, SimulatorAction, SimulatorCall, SimulatorClientError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub(crate) fn reply(action: &str, payload: &Value, state: &Arc<Mutex<Ocpp16State>>) -> Value {
    let handle = state
        .lock()
        .expect("native state lock")
        .reservation16
        .clone();
    let now = OffsetDateTime::now_utc();
    let result = match action {
        "ReserveNow" => match ReserveRequest::deserialize(payload) {
            Ok(request) => {
                if !super::model::valid(&request) {
                    return json!({"callError":"FormationViolation"});
                }
                let Some(handle) = handle else {
                    return json!({"status":"Rejected"});
                };
                let connector = request.connector_id;
                handle.reserve(request, now).map(|status| {
                    if status == ReserveStatus::Accepted && connector != 0 {
                        notify(state, connector, "Reserved");
                    }
                    json!({"status":status})
                })
            }
            Err(_) => return json!({"callError":"FormationViolation"}),
        },
        "CancelReservation" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase", deny_unknown_fields)]
            struct Cancel {
                reservation_id: i32,
            }
            let Ok(cancel) = Cancel::deserialize(payload) else {
                return json!({"callError":"FormationViolation"});
            };
            let Some(handle) = handle else {
                return json!({"status":"Rejected"});
            };
            let connector = handle.snapshot()["reservations"]
                .as_array()
                .and_then(|rows| {
                    rows.iter()
                        .find(|r| r["reservationId"] == cancel.reservation_id)
                        .and_then(|r| r["connectorId"].as_u64())
                });
            handle.cancel(cancel.reservation_id, now).map(|accepted| {
                if accepted && let Some(connector) = connector.filter(|id| *id != 0) {
                    notify(
                        state,
                        u32::try_from(connector).expect("native connector"),
                        "Available",
                    );
                }
                json!({"status":if accepted {"Accepted"} else {"Rejected"}})
            })
        }
        _ => unreachable!(),
    };
    result.unwrap_or_else(|_| json!({"callError":"InternalError"}))
}

fn notify(state: &Arc<Mutex<Ocpp16State>>, connector: u32, status: &'static str) {
    let mut state = state.lock().expect("native state lock");
    if let Some(prior) = state
        .reservation_notifications
        .iter_mut()
        .find(|(id, _)| *id == connector)
    {
        prior.1 = status;
    } else {
        state.reservation_notifications.push((connector, status));
    }
}

pub(crate) fn expire(state: &Arc<Mutex<Ocpp16State>>) {
    let handle = state
        .lock()
        .expect("native state lock")
        .reservation16
        .clone();
    if let Some(handle) = handle {
        if let Ok(connectors) = handle.expire(OffsetDateTime::now_utc()) {
            for connector in connectors {
                notify(state, connector, "Available");
            }
        }
        let mut current = state.lock().expect("native state lock");
        if current.registered
            && current.socket_connected
            && let Some(sender) = current
                .notifications
                .as_ref()
                .and_then(tokio::sync::mpsc::WeakSender::upgrade)
        {
            current
                .reservation_notifications
                .retain(|(connector, status)| {
                    sender
                        .try_send(Command::ReservationStatus(*connector, status))
                        .is_err()
                });
        }
    }
}

#[allow(clippy::too_many_lines)] // Each native action checks the actual model before it reaches the socket.
pub(crate) async fn prepare(
    client: &ocpp_client::ocpp_1_6::OCPP1_6Client,
    state: &Arc<Mutex<Ocpp16State>>,
    call: &mut SimulatorCall,
    generation: u64,
) -> Result<(), SimulatorClientError> {
    let (reservation, local) = {
        let state = state.lock().expect("native state lock");
        (state.reservation16.clone(), state.local.clone())
    };
    let Some(reservation) = reservation else {
        return Ok(());
    };
    let error = |code: &'static str| SimulatorClientError::Protocol(code.to_owned());
    if matches!(
        call.action,
        SimulatorAction::StartTransaction | SimulatorAction::StatusNotification
    ) {
        crate::client_exchange16::NativeExchange::recovery(generation, true)
            .validate_recovery(state)?;
    }
    match call.action {
        SimulatorAction::StatusNotification => {
            serde_json::from_value::<ocpp_client::ocpp_types::v16::StatusNotificationRequest>(
                call.payload.clone(),
            )
            .map_err(|_| error("reservation_status_invalid"))?;
            let connector = call.payload["connectorId"]
                .as_u64()
                .and_then(|id| u32::try_from(id).ok())
                .ok_or_else(|| error("reservation_connector_invalid"))?;
            let status = match call.payload["status"].as_str() {
                Some("Available") => ConnectorState::Available,
                Some("Faulted") => ConnectorState::Faulted,
                Some("Unavailable") => ConnectorState::Unavailable,
                Some("Reserved") => return Ok(()),
                Some(_) => ConnectorState::Occupied,
                None => return Err(error("reservation_status_invalid")),
            };
            reservation
                .set_connector(connector, status)
                .map_err(error)?;
        }
        SimulatorAction::StartTransaction => {
            crate::local_authorization::wire::StartRequest::deserialize(&call.payload)
                .map_err(|_| error("reservation_start_invalid"))?;
            if call.payload["timestamp"]
                .as_str()
                .is_none_or(|value| OffsetDateTime::parse(value, &Rfc3339).is_err())
                || call.payload["idTag"]
                    .as_str()
                    .is_none_or(|token| token.chars().count() > 20)
            {
                return Err(error("reservation_start_invalid"));
            }
            let connector = call.payload["connectorId"]
                .as_u64()
                .and_then(|id| u32::try_from(id).ok())
                .ok_or_else(|| error("reservation_connector_invalid"))?;
            let token = call.payload["idTag"]
                .as_str()
                .ok_or_else(|| error("reservation_identity_invalid"))?;
            let local = local.ok_or_else(|| error("reservation_authorization_unavailable"))?;
            let info = if let Some(info) = local.identity_info(token) {
                info
            } else {
                let authorization = SimulatorCall {
                    action: SimulatorAction::Authorize,
                    payload: json!({"idTag":token}),
                };
                let exchange = crate::client_exchange16::NativeExchange::recovery(generation, true);
                let response = exchange.send(client, &authorization).await?;
                let info =
                    crate::local_authorization::NativeInfo::deserialize(&response["idTagInfo"])
                        .map_err(|_| error("reservation_authorization_invalid"))?;
                if state.lock().expect("native state lock").socket_generation != generation {
                    return Err(error("reservation_authorization_generation_unavailable"));
                }
                local.observe_central(token, info.clone()).map_err(error)?;
                info
            };
            let now = OffsetDateTime::now_utc();
            let authorized = info.status
                == ocpp_client::ocpp_types::v16::common::IdTagInfoStatus::Accepted
                && info.expiry_date.as_ref().is_none_or(|expiry| {
                    OffsetDateTime::parse(expiry, &Rfc3339).is_ok_and(|expiry| expiry > now)
                });
            let expected = call
                .payload
                .get("reservationId")
                .map(|value| {
                    value
                        .as_i64()
                        .and_then(|id| i32::try_from(id).ok())
                        .ok_or_else(|| error("reservation_start_id_invalid"))
                })
                .transpose()?;
            let current = state.lock().expect("native state lock");
            if current.socket_generation != generation
                || !current.socket_connected
                || !current.registered
            {
                return Err(error("reservation_start_generation_unavailable"));
            }
            let matched = reservation
                .start_expected(
                    connector,
                    token,
                    info.parent_id_tag.as_deref(),
                    authorized,
                    now,
                    expected,
                )
                .map_err(error)?;
            if let Some(id) = matched {
                call.payload["reservationId"] = id.into();
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) async fn status(
    client: &ocpp_client::ocpp_1_6::OCPP1_6Client,
    state: &Arc<Mutex<Ocpp16State>>,
    connector: u32,
    status: &'static str,
) -> Result<(), SimulatorClientError> {
    let call = SimulatorCall {
        action: SimulatorAction::StatusNotification,
        payload: json!({"connectorId":connector,"errorCode":"NoError","status":status}),
    };
    let exchange = crate::client_exchange16::NativeExchange::capture(state, &call);
    exchange.send(client, &call).await.map(|_| ())
}
