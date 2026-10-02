use super::{FRAME_LIMIT, Peer, Reply};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub async fn receive(socket: &mut Socket) -> io::Result<Value> {
    loop {
        let message = socket
            .next()
            .await
            .ok_or_else(|| io::Error::other("independent peer disconnected"))?
            .map_err(|_| io::Error::other("independent peer receive failed"))?;
        match message {
            Message::Text(text) if text.len() <= FRAME_LIMIT => {
                return serde_json::from_str(&text)
                    .map_err(|_| io::Error::other("independent peer invalid JSON"));
            }
            Message::Ping(_) | Message::Pong(_) => {}
            _ => return Err(io::Error::other("independent peer invalid frame")),
        }
    }
}

pub async fn send(socket: &mut Socket, value: Value) -> io::Result<()> {
    let text = serde_json::to_string(&value)
        .map_err(|_| io::Error::other("independent peer encoding failed"))?;
    if text.len() > FRAME_LIMIT {
        return Err(io::Error::other("independent peer frame exceeds bound"));
    }
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| io::Error::other("independent peer send failed"))
}

pub fn envelope(call: &Value, reply: Reply) -> io::Result<Value> {
    if call.as_array().is_none_or(|a| a.len() != 4)
        || call[0] != 2
        || !call[1]
            .as_str()
            .is_some_and(|id| !id.is_empty() && id.len() <= 36)
        || call[2].as_str().is_none()
        || !call[3].is_object()
    {
        return Err(io::Error::other("independent peer invalid CALL"));
    }
    Ok(match reply {
        Reply::Result(payload) => json!([3, call[1], payload]),
        Reply::Error(code) => json!([4, call[1], code, "", {}]),
    })
}

pub(super) fn variables(payload: &Value, phases: &BTreeMap<u32, bool>) -> Reply {
    let Some(queries) = payload["getVariableData"].as_array() else {
        return Reply::Error("FormationViolation");
    };
    if !super::validation::keys(payload, &["getVariableData"])
        || queries.is_empty()
        || queries.len() > 128
        || queries.iter().any(|query| {
            !super::validation::keys(query, &["component", "variable", "attributeType"])
                || !super::validation::keys(&query["component"], &["name", "instance", "evse"])
                || !super::validation::keys(&query["variable"], &["name", "instance"])
                || !query["component"]["name"]
                    .as_str()
                    .is_some_and(|n| !n.is_empty() && n.chars().count() <= 50)
                || !query["variable"]["name"]
                    .as_str()
                    .is_some_and(|n| !n.is_empty() && n.chars().count() <= 50)
        })
    {
        return Reply::Error("FormationViolation");
    }
    let results: Vec<_> = queries
        .iter()
        .map(|query| {
            let component = &query["component"];
            let variable = &query["variable"];
            let evse = component["evse"]["id"]
                .as_u64()
                .and_then(|id| u32::try_from(id).ok());
            let exact = component["name"] == "SmartChargingCtrlr"
                && variable["name"] == "ACPhaseSwitchingSupported"
                && component.get("instance").is_none()
                && variable.get("instance").is_none()
                && component["evse"].get("connectorId").is_none()
                && super::validation::keys(&component["evse"], &["id"])
                && evse.is_some_and(|id| id > 0)
                && query
                    .get("attributeType")
                    .is_none_or(|value| value == "Actual");
            let supported = evse.and_then(|id| phases.get(&id)).filter(|_| exact);
            let mut result = json!({"component":component,"variable":variable,
            "attributeType":"Actual","attributeStatus":"UnknownVariable"});
            if let Some(supported) = supported {
                result["attributeStatus"] = json!("Accepted");
                result["attributeValue"] = json!(if *supported { "true" } else { "false" });
            }
            result
        })
        .collect();
    Reply::Result(json!({"getVariableResult":results}))
}

impl Peer {
    pub async fn serve_one(&mut self, socket: &mut Socket) -> io::Result<Value> {
        let call = receive(socket).await?;
        envelope(&call, Reply::Error("FormationViolation"))?;
        let reply = self.apply(call[2].as_str().unwrap(), &call[3])?;
        if std::mem::take(&mut self.controls.disconnect_after_apply) {
            socket
                .close(None)
                .await
                .map_err(|_| io::Error::other("independent peer close failed"))?;
            return Ok(call);
        }
        tokio::time::sleep(self.controls.delay).await;
        send(socket, envelope(&call, reply)?).await?;
        Ok(call)
    }

    pub async fn boot(socket: &mut Socket) -> io::Result<Value> {
        station_call(
            socket,
            "BootNotification",
            json!({"reason":"PowerUp",
            "chargingStation":{"model":"Profile201Peer","vendorName":"ProjectTest"}}),
        )
        .await
    }

    pub async fn transaction(
        &mut self,
        socket: &mut Socket,
        evse: u32,
        id: &str,
        ended: bool,
    ) -> io::Result<Value> {
        if evse == 0 || i32::try_from(evse).is_err() || id.is_empty() || id.chars().count() > 36 {
            return Err(io::Error::other("invalid independent transaction"));
        }
        let timestamp = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|_| io::Error::other("independent timestamp failed"))?;
        let response = station_call(socket, "TransactionEvent", json!({
            "eventType":if ended {"Ended"} else {"Started"},"timestamp":timestamp,
            "triggerReason":if ended {"StopAuthorized"} else {"CablePluggedIn"},
            "seqNo":i32::from(ended),"evse":{"id":evse,"connectorId":1},
            "transactionInfo":{"transactionId":id,"chargingState":if ended {"Idle"} else {"Charging"}}
        })).await?;
        if ended {
            let profiles: Vec<_> = self
                .profiles
                .iter()
                .filter(|payload| {
                    !(payload["evseId"] == evse
                        && payload["chargingProfile"]["chargingProfilePurpose"] == "TxProfile"
                        && payload["chargingProfile"]["transactionId"] == id)
                })
                .cloned()
                .collect();
            super::state::persist(&self.path, &profiles)?;
            self.profiles = profiles;
        }
        self.set_transaction(evse, if ended { None } else { Some(id) });
        Ok(response)
    }
}

async fn station_call(socket: &mut Socket, action: &str, payload: Value) -> io::Result<Value> {
    let id = uuid::Uuid::new_v4().to_string();
    send(socket, json!([2, id, action, payload])).await?;
    let response = receive(socket).await?;
    if response[0] != 3
        || response[1] != id
        || response.as_array().is_none_or(|a| a.len() != 3)
        || !response[2].is_object()
    {
        return Err(io::Error::other("independent peer station CALL failed"));
    }
    Ok(response[2].clone())
}
