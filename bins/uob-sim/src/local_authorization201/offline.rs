use super::{
    Delivery, IdToken, LocalAuthorization201Handle, OFFLINE_LIMIT, OfflineRecord, validation,
};
use crate::{ProtocolClient, SimulatorAction, SimulatorCall};
use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

impl LocalAuthorization201Handle {
    /// Records an original Started fact only after native offline authorization.
    /// # Errors
    /// Returns value-free errors for invalid facts, occupied EVSE, capacity or failed persistence.
    pub fn offline_start(&self, payload: Value) -> Result<bool, &'static str> {
        if !validation::valid_event(&payload)
            || payload["eventType"] != "Started"
            || payload["offline"] != true
            || payload["seqNo"] != 0
        {
            return Err("invalid_offline_start");
        }
        let token: IdToken = serde_json::from_value(payload["idToken"].clone())
            .map_err(|_| "invalid_offline_token")?;
        let evse = payload
            .pointer("/evse/id")
            .and_then(Value::as_i64)
            .and_then(|id| i32::try_from(id).ok())
            .ok_or("invalid_offline_evse")?;
        let timestamp = payload["timestamp"]
            .as_str()
            .ok_or("invalid_offline_timestamp")?;
        let time =
            OffsetDateTime::parse(timestamp, &Rfc3339).map_err(|_| "invalid_offline_timestamp")?;
        if !self.authorize_offline(&token, evse, time) {
            return Ok(false);
        }
        let transaction = payload
            .pointer("/transactionInfo/transactionId")
            .and_then(Value::as_str)
            .ok_or("invalid_offline_transaction")?;
        let mut model = self.0.lock();
        if model.state.active.contains_key(transaction)
            || model
                .state
                .active
                .values()
                .any(|value| value.pointer("/evse/id") == payload.pointer("/evse/id"))
        {
            return Err("offline_transaction_active");
        }
        if model.state.offline.len() >= OFFLINE_LIMIT {
            return Err("offline_record_capacity");
        }
        let mut next = model.state.clone();
        next.active.insert(transaction.to_owned(), payload.clone());
        next.offline.push(OfflineRecord {
            payload,
            delivery: Delivery::Pending,
        });
        model.commit(next)?;
        Ok(true)
    }
    /// Retains the original Ended fact with the same transaction/EVSE and next sequence.
    /// # Errors
    /// Returns value-free errors for invalid or unmatched facts, capacity or failed persistence.
    pub fn offline_stop(&self, payload: Value) -> Result<(), &'static str> {
        if !validation::valid_event(&payload)
            || payload["eventType"] != "Ended"
            || payload["offline"] != true
        {
            return Err("invalid_offline_stop");
        }
        let transaction = payload
            .pointer("/transactionInfo/transactionId")
            .and_then(Value::as_str)
            .ok_or("invalid_offline_transaction")?;
        let mut model = self.0.lock();
        let original = model
            .state
            .active
            .get(transaction)
            .ok_or("offline_transaction_missing")?;
        if payload.get("evse") != original.get("evse")
            || payload["seqNo"].as_i64()
                != original["seqNo"].as_i64().and_then(|n| n.checked_add(1))
        {
            return Err("invalid_offline_sequence");
        }
        if model.state.offline.len() >= OFFLINE_LIMIT {
            return Err("offline_record_capacity");
        }
        let mut next = model.state.clone();
        next.active.remove(transaction);
        next.offline.push(OfflineRecord {
            payload,
            delivery: Delivery::Pending,
        });
        model.commit(next)
    }
    /// Delivers only persisted original facts after the current socket's Accepted Boot.
    /// Uncertain sends are retained and never automatically retried.
    /// # Errors
    /// Returns value-free errors for denied registration, send uncertainty or durable failure.
    pub async fn replay(&self, client: &dyn ProtocolClient) -> Result<(), &'static str> {
        self.replay_with(
            |payload: Value| async move {
                let action = match payload["eventType"].as_str() {
                    Some("Started") => SimulatorAction::StartTransaction,
                    Some("Ended") => SimulatorAction::StopTransaction,
                    _ => SimulatorAction::MeterValues,
                };
                client.call(SimulatorCall { action, payload }).await.is_ok()
            },
            || {
                client.version() == crate::OcppVersion::V2_0_1
                    && client.accepted_registration() == Some(true)
                    && client.socket_connected() == Some(true)
            },
        )
        .await
    }

    pub(super) async fn replay_with<F, Fut, C>(
        &self,
        mut send: F,
        registered: C,
    ) -> Result<(), &'static str>
    where
        F: FnMut(Value) -> Fut,
        Fut: std::future::Future<Output = bool>,
        C: Fn() -> bool,
    {
        let _serial = self.1.lock().await;
        loop {
            if !registered() {
                return Err("station_not_registered");
            }
            let payload = {
                let mut model = self.0.lock();
                let Some(first) = model.state.offline.first() else {
                    return Ok(());
                };
                if first.delivery != Delivery::Pending {
                    return Err("offline_replay_uncertain");
                }
                let payload = first.payload.clone();
                let mut next = model.state.clone();
                next.offline[0].delivery = Delivery::Sending;
                model.commit(next)?;
                payload
            };
            let confirmed = send(payload).await;
            let mut model = self.0.lock();
            let mut next = model.state.clone();
            if !confirmed || !registered() {
                next.offline[0].delivery = Delivery::Uncertain;
                model.commit(next)?;
                return Err("offline_replay_uncertain");
            }
            next.offline.remove(0);
            model.commit(next)?;
        }
    }
}
