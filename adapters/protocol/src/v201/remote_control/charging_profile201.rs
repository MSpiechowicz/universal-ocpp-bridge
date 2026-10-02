use super::{RemoteControlSession, charging_limit, mapping};
use crate::command_registry::charging_profile201::{self as registry, Request};
use serde::Deserialize;
use serde_json::Value;
use uob_application::{
    CommandDispatchOutcome, ProfileFootprint201, ProfileMutation201, ProfileReservation201,
};
use uob_contracts::{
    ChargingProfileReason201, ChargingProfileResult201, ClearChargingProfileStatus201, Command,
    CommandErrorCode, CommandOperation, Operation, ProtocolEdition, SetChargingProfileStatus201,
    StationSnapshot, TransactionState, UtcTimestamp,
};

impl RemoteControlSession {
    pub(super) fn learn_transactions(
        &self,
        previous: &StationSnapshot,
        next: &StationSnapshot,
    ) -> Result<(), uob_application::StationCommandError> {
        let mut established = self
            .established_transactions
            .lock()
            .map_err(|_| super::state_error())?;
        for tx in &next.transactions {
            let Some(state) = tx
                .protocol_state
                .as_ref()
                .filter(|state| state.protocol == ProtocolEdition::Ocpp201)
            else {
                continue;
            };
            if tx.state == TransactionState::Ended {
                established.remove(&state.native_transaction_id);
            } else if matches!(
                tx.state,
                TransactionState::Pending | TransactionState::Active | TransactionState::Suspended
            ) && previous
                .transactions
                .iter()
                .find(|old| old.transaction_id == tx.transaction_id)
                .is_none_or(|old| old.protocol_state != tx.protocol_state)
                && (established.contains(&state.native_transaction_id) || established.len() < 64)
            {
                established.insert(state.native_transaction_id.clone());
            }
        }
        Ok(())
    }
    pub(super) fn profile_expectation(
        &self,
        command: &Command<Value>,
        generation: Option<u64>,
        now: UtcTimestamp,
    ) -> Result<Option<ProfileReservation201>, CommandErrorCode> {
        let snapshot = self
            .snapshot
            .read()
            .map_err(|_| CommandErrorCode::PolicyRejected)?;
        let Some((_, mutation)) = self.profile_context(command, &snapshot, now)? else {
            return Ok(None);
        };
        Ok(Some(ProfileReservation201 {
            request_id: command.request_id.clone(),
            station: snapshot.station.clone(),
            connection: self.handle.connection_id(),
            generation: generation.unwrap_or(0),
            requires_baseline: snapshot.capabilities.supports(&Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp201,
                action: "SetChargingProfile".to_owned(),
            }),
            mutation,
        }))
    }
    /// The native request context is separate from the canonical `SetChargingProfile` wire action.
    pub(super) fn profile_context(
        &self,
        command: &Command<Value>,
        snapshot: &StationSnapshot,
        now: UtcTimestamp,
    ) -> Result<Option<(Option<Request>, ProfileMutation201)>, CommandErrorCode> {
        let phases = self
            .phase
            .lock()
            .map_err(|_| CommandErrorCode::PolicyRejected)?;
        self.profile_context_with_phase(command, snapshot, now, &phases)
    }
    pub(super) fn profile_context_with_phase(
        &self,
        command: &Command<Value>,
        snapshot: &StationSnapshot,
        now: UtcTimestamp,
        phases: &super::phase_capability::PhaseCapabilities,
    ) -> Result<Option<(Option<Request>, ProfileMutation201)>, CommandErrorCode> {
        match &command.operation {
            CommandOperation::Ocpp(operation)
                if operation.protocol == ProtocolEdition::Ocpp201
                    && registry::ACTIONS.contains(&operation.action.as_str()) =>
            {
                self.ensure_profile_commit_complete()?;
                let request = registry::validate(&command.resource, operation)?;
                let mutation =
                    self.native_profile_context(command, snapshot, now, phases, &request)?;
                Ok(Some((Some(request), mutation)))
            }
            CommandOperation::SetChargingLimit(limit) => {
                let mutation = self.canonical_profile_context(command, snapshot, limit, phases)?;
                Ok(Some((None, mutation)))
            }
            _ => Ok(None),
        }
    }

    fn ensure_profile_commit_complete(&self) -> Result<(), CommandErrorCode> {
        if self
            .pending_snapshot_commit
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(CommandErrorCode::PolicyRejected);
        }
        Ok(())
    }

    fn canonical_profile_context(
        &self,
        command: &Command<Value>,
        snapshot: &StationSnapshot,
        limit: &uob_contracts::ChargingLimit,
        phases: &super::phase_capability::PhaseCapabilities,
    ) -> Result<ProfileMutation201, CommandErrorCode> {
        self.ensure_profile_commit_complete()?;
        if !phases.active() {
            return Err(CommandErrorCode::StationDisconnected);
        }
        let invalid = CommandErrorCode::InvalidParameters;
        let payload = charging_limit::prepare(command, snapshot, limit)?;
        let evse =
            i32::try_from(payload["evseId"].as_i64().ok_or(invalid)?).map_err(|_| invalid)?;
        let request = registry::parse_set(evse, &payload).ok_or(invalid)?;
        Ok(ProfileMutation201::Set {
            footprint: ProfileFootprint201::from(&request),
            full_native: false,
        })
    }

    fn native_profile_context(
        &self,
        command: &Command<Value>,
        snapshot: &StationSnapshot,
        now: UtcTimestamp,
        phases: &super::phase_capability::PhaseCapabilities,
        request: &Request,
    ) -> Result<ProfileMutation201, CommandErrorCode> {
        let invalid = CommandErrorCode::InvalidParameters;
        let capabilities = mapping::capabilities(snapshot, &command.resource).ok_or(invalid)?;
        command
            .validate_for_dispatch(capabilities, now)
            .map_err(|error| match error {
                uob_contracts::CommandValidationError::Expired => CommandErrorCode::Expired,
                uob_contracts::CommandValidationError::UnsupportedOperation(_) => {
                    CommandErrorCode::UnsupportedOperation
                }
            })?;
        uob_application::registration::v201::accepted(snapshot)
            .map_err(|_| CommandErrorCode::PolicyRejected)?;
        if self.handle.is_closed() || !phases.active() {
            return Err(CommandErrorCode::StationDisconnected);
        }
        match request {
            Request::Set(request) => {
                self.validate_profile_transaction(command, snapshot, request)?;
                if request
                    .charging_profile
                    .charging_schedule
                    .iter()
                    .any(|schedule| {
                        schedule
                            .charging_schedule_period
                            .iter()
                            .any(|period| period.phase_to_use.is_some())
                    })
                    && !phases.supports(request.evse_id)
                {
                    return Err(CommandErrorCode::PolicyRejected);
                }
                Ok(ProfileMutation201::Set {
                    footprint: ProfileFootprint201::from(request),
                    full_native: true,
                })
            }
            Request::Clear(request) => Ok(ProfileMutation201::Clear(request.clone())),
        }
    }

    fn validate_profile_transaction(
        &self,
        command: &Command<Value>,
        snapshot: &StationSnapshot,
        request: &uob_contracts::SetChargingProfileRequest201,
    ) -> Result<(), CommandErrorCode> {
        let Some(id) = &request.charging_profile.transaction_id else {
            return Ok(());
        };
        let invalid = CommandErrorCode::InvalidParameters;
        if !self
            .established_transactions
            .lock()
            .map_err(|_| CommandErrorCode::PolicyRejected)?
            .contains(id)
        {
            return Err(invalid);
        }
        let mut eligible = snapshot.transactions.iter().filter(|tx| {
            matches!(
                tx.state,
                TransactionState::Pending | TransactionState::Active | TransactionState::Suspended
            ) && tx.resource.bridge_id == command.resource.bridge_id
                && tx.resource.station_id == command.resource.station_id
                && matches!(tx.resource.native_protocol_reference,
                    Some(uob_contracts::NativeProtocolReference::Ocpp201 { evse_id, .. })
                        if i32::try_from(evse_id).ok() == Some(request.evse_id))
                && tx
                    .protocol_state
                    .as_ref()
                    .is_some_and(|state| state.protocol == ProtocolEdition::Ocpp201)
        });
        let tx = eligible.next().ok_or(invalid)?;
        if eligible.next().is_some()
            || tx
                .protocol_state
                .as_ref()
                .is_none_or(|state| &state.native_transaction_id != id)
        {
            return Err(invalid);
        }
        // Duplicate native transaction IDs elsewhere are ambiguous, even across EVSEs.
        if snapshot
            .transactions
            .iter()
            .filter(|tx| {
                tx.state != TransactionState::Ended
                    && tx.protocol_state.as_ref().is_some_and(|state| {
                        state.protocol == ProtocolEdition::Ocpp201
                            && &state.native_transaction_id == id
                    })
            })
            .count()
            != 1
        {
            return Err(invalid);
        }
        Ok(())
    }
}

pub(super) fn response(request: Request, payload: &Value) -> CommandDispatchOutcome {
    let (index, result) = match request {
        Request::Set(request) => {
            let Some(status) = SetChargingProfileStatus201::deserialize(&payload["status"]).ok()
            else {
                return mapping::uncertain();
            };
            (
                1,
                ChargingProfileResult201::SetChargingProfile {
                    request,
                    status,
                    reason_code: reason(payload),
                },
            )
        }
        Request::Clear(request) => {
            let Some(status) = ClearChargingProfileStatus201::deserialize(&payload["status"]).ok()
            else {
                return mapping::uncertain();
            };
            (
                3,
                ChargingProfileResult201::ClearChargingProfile {
                    request,
                    status,
                    reason_code: reason(payload),
                },
            )
        }
    };
    if !registry::valid_schema(index, payload) {
        return mapping::uncertain();
    }
    CommandDispatchOutcome::ChargingProfileResponse201(result)
}
fn reason(payload: &Value) -> Option<ChargingProfileReason201> {
    ChargingProfileReason201::deserialize(&payload["statusInfo"]["reasonCode"]).ok()
}
