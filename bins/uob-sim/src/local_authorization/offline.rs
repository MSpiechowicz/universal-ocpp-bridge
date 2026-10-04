use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{LocalAuthorization, LocalAuthorizationHandle, OFFLINE_LIMIT};
use crate::{ProtocolClient, SimulatorAction, SimulatorCall, SimulatorClientError};

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NativeStart {
    connector_id: u16,
    id_tag: String,
    meter_start: i64,
    timestamp: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NativeStop {
    meter_stop: i64,
    timestamp: String,
    #[serde(default = "local_reason")]
    reason: ocpp_client::ocpp_types::v16::common::Reason,
}

fn local_reason() -> ocpp_client::ocpp_types::v16::common::Reason {
    ocpp_client::ocpp_types::v16::common::Reason::Local
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReplayState {
    Pending,
    StartInFlight,
    StartUncertain,
    Active { transaction_id: i64 },
    StopInFlight { transaction_id: i64 },
    StopUncertain { transaction_id: i64 },
}

impl ReplayState {
    pub(super) fn is_uncertain(&self) -> bool {
        matches!(
            self,
            Self::StartInFlight
                | Self::StartUncertain
                | Self::StopInFlight { .. }
                | Self::StopUncertain { .. }
        )
    }

    pub(super) fn recover_uncertain(&mut self) {
        *self = match *self {
            Self::StartInFlight => Self::StartUncertain,
            Self::StopInFlight { transaction_id } => Self::StopUncertain { transaction_id },
            _ => return,
        };
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OfflineRecord {
    local_id: u64,
    start: NativeStart,
    stop: Option<NativeStop>,
    pub replay: ReplayState,
}

impl LocalAuthorizationHandle {
    /// Start charging only after native offline authorization and durable queuing.
    ///
    /// # Errors
    /// Returns a value-free error for invalid input, absent persistent storage,
    /// unavailable state, active connector, exhausted capacity, or failed I/O.
    pub fn offline_start(&self, payload: serde_json::Value) -> Result<bool, &'static str> {
        let start: NativeStart =
            serde_json::from_value(payload).map_err(|_| "invalid_offline_start")?;
        if start.connector_id == 0
            || start.id_tag.chars().count() > 20
            || OffsetDateTime::parse(&start.timestamp, &Rfc3339).is_err()
        {
            return Err("invalid_offline_start");
        }
        let mut model = self.0.lock();
        if model.storage.is_none() {
            return Err("persistent_native_state_required");
        }
        if model.unavailable {
            return Err("private_state_unavailable");
        }
        if !model.authorize_offline(&start.id_tag, OffsetDateTime::now_utc()) {
            return Ok(false);
        }
        if model.state.offline.len() == OFFLINE_LIMIT {
            return Err("offline_capacity");
        }
        if model
            .state
            .offline
            .iter()
            .any(|record| record.start.connector_id == start.connector_id && record.stop.is_none())
        {
            return Err("offline_transaction_active");
        }
        let mut next = model.state.clone();
        let local_id = next.next_offline_id;
        next.next_offline_id = local_id.checked_add(1).ok_or("offline_identity_capacity")?;
        next.offline.push(OfflineRecord {
            local_id,
            start,
            stop: None,
            replay: ReplayState::Pending,
        });
        model.commit(next)?;
        Ok(true)
    }

    /// Durably record a stop against the original offline start.
    ///
    /// # Errors
    /// Returns a value-free error for invalid input, unknown active connector,
    /// unavailable state, or failed durable I/O.
    pub fn offline_stop(
        &self,
        connector: u16,
        payload: serde_json::Value,
    ) -> Result<(), &'static str> {
        let stop: NativeStop =
            serde_json::from_value(payload).map_err(|_| "invalid_offline_stop")?;
        OffsetDateTime::parse(&stop.timestamp, &Rfc3339).map_err(|_| "invalid_offline_stop")?;
        let mut model = self.0.lock();
        let mut next = model.state.clone();
        let record = next
            .offline
            .iter_mut()
            .find(|record| record.start.connector_id == connector && record.stop.is_none())
            .ok_or("offline_transaction_missing")?;
        record.stop = Some(stop);
        model.commit(next)
    }

    /// Replay original native records, retaining uncertain sends without retry.
    ///
    /// # Errors
    /// Returns a value-free protocol error for unavailable/failed private state
    /// or an uncertain native exchange. Persisted uncertain records are never
    /// automatically retransmitted.
    ///
    /// # Panics
    /// Panics only if a private replay record violates the validated internal
    /// state invariant while its exclusive model lock is held.
    pub async fn replay(&self, client: &dyn ProtocolClient) -> Result<(), SimulatorClientError> {
        let _replay_guard = self.1.lock().await;
        loop {
            let next_call = {
                let mut model = self.0.lock();
                if model.unavailable {
                    return Err(private_error("private_state_unavailable"));
                }
                let mut next = model.state.clone();
                let Some(record) = next.offline.iter_mut().find(|record| {
                    matches!(record.replay, ReplayState::Pending)
                        || matches!(record.replay, ReplayState::Active { .. })
                            && record.stop.is_some()
                }) else {
                    return Ok(());
                };
                let id = record.local_id;
                let call = match record.replay {
                    ReplayState::Pending => {
                        record.replay = ReplayState::StartInFlight;
                        SimulatorCall {
                            action: SimulatorAction::StartTransaction,
                            payload: serde_json::to_value(&record.start)
                                .map_err(|_| private_error("offline_encoding"))?,
                        }
                    }
                    ReplayState::Active { transaction_id } => {
                        let stop = record.stop.as_ref().expect("selected offline stop");
                        record.replay = ReplayState::StopInFlight { transaction_id };
                        SimulatorCall {
                            action: SimulatorAction::StopTransaction,
                            payload: serde_json::json!({"transactionId":transaction_id, "meterStop":stop.meter_stop,
                                "timestamp":stop.timestamp, "idTag":record.start.id_tag, "reason":stop.reason}),
                        }
                    }
                    _ => unreachable!(),
                };
                model.commit(next).map_err(private_error)?;
                (id, call)
            };
            let (local_id, call) = next_call;
            let response = client.call(call).await;
            let mut model = self.0.lock();
            let mut next = model.state.clone();
            let record = next
                .offline
                .iter_mut()
                .find(|record| record.local_id == local_id)
                .expect("retained offline record");
            match (&record.replay, &response) {
                (ReplayState::StartInFlight, Ok(response)) => {
                    if let Some(transaction_id) = response
                        .get("transactionId")
                        .and_then(serde_json::Value::as_i64)
                    {
                        record.replay = ReplayState::Active { transaction_id };
                        if record.stop.is_none()
                            && response
                                .pointer("/idTagInfo/status")
                                .and_then(serde_json::Value::as_str)
                                != Some("Accepted")
                        {
                            record.stop = Some(NativeStop {
                                meter_stop: record.start.meter_start,
                                timestamp: OffsetDateTime::now_utc()
                                    .format(&Rfc3339)
                                    .map_err(|_| private_error("offline_timestamp"))?,
                                reason: ocpp_client::ocpp_types::v16::common::Reason::DeAuthorized,
                            });
                        }
                    } else {
                        record.replay = ReplayState::StartUncertain;
                    }
                }
                (ReplayState::StopInFlight { .. }, Ok(_)) => {
                    next.offline.retain(|record| record.local_id != local_id);
                }
                _ => record.replay.recover_uncertain(),
            }
            model.commit(next).map_err(private_error)?;
            if response.is_err() {
                return Err(private_error("offline_replay_uncertain"));
            }
        }
    }

    pub(crate) fn stop_for_reset(
        &self,
        reason: &ocpp_client::ocpp_types::v16::common::Reason,
    ) -> Result<(), &'static str> {
        let mut model = self.0.lock();
        if !model
            .state
            .offline
            .iter()
            .any(|record| record.stop.is_none())
        {
            return Ok(());
        }
        let timestamp = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|_| "native_reset_timestamp")?;
        let mut next = model.state.clone();
        for record in &mut next.offline {
            if record.stop.is_none() {
                record.stop = Some(NativeStop {
                    meter_stop: record.start.meter_start,
                    timestamp: timestamp.clone(),
                    reason: reason.clone(),
                });
            }
        }
        model.commit(next)
    }

    pub(crate) fn active_connector(&self, transaction_id: i64) -> Option<u16> {
        self.0.lock().state.offline.iter().find_map(|record| {
            if record.stop.is_none() && matches!(record.replay, ReplayState::Active { transaction_id: id } if id == transaction_id) {
                Some(record.start.connector_id)
            } else { None }
        })
    }

    pub(crate) fn connector_busy(&self, connector: u16) -> bool {
        self.0
            .lock()
            .state
            .offline
            .iter()
            .any(|record| record.stop.is_none() && record.start.connector_id == connector)
    }
}

impl LocalAuthorization {
    pub(super) fn validate_offline(&self) -> Result<(), &'static str> {
        if self.state.next_offline_id == 0 {
            return Err("private_state_invalid");
        }
        let mut ids = HashSet::new();
        let mut active_connectors = HashSet::new();
        for record in &self.state.offline {
            if record.local_id == 0
                || record.local_id >= self.state.next_offline_id
                || !ids.insert(record.local_id)
                || record.start.connector_id == 0
                || record.start.id_tag.chars().count() > 20
                || OffsetDateTime::parse(&record.start.timestamp, &Rfc3339).is_err()
                || record
                    .stop
                    .as_ref()
                    .is_some_and(|stop| OffsetDateTime::parse(&stop.timestamp, &Rfc3339).is_err())
                || record.stop.is_none() && !active_connectors.insert(record.start.connector_id)
            {
                return Err("private_state_invalid");
            }
        }
        Ok(())
    }
}

fn private_error(code: &'static str) -> SimulatorClientError {
    SimulatorClientError::Protocol(code.to_owned())
}

impl Drop for NativeStart {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.id_tag);
    }
}
