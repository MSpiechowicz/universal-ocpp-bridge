use std::{
    collections::BTreeMap,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use serde_json::Value;
use uob_application::{
    CommandDispatchOutcome, StationCommandContext, StationCommandError, StationCommandFuture,
    StationCommandPort,
};
use uob_contracts::{Command, Connectivity, ResourceRef, StationId, StationSnapshot};
use uob_protocol_adapter::{v16, v201};

enum Session {
    V16(Arc<v16::remote_control::RemoteControlSession>),
    V201(Arc<v201::remote_control::RemoteControlSession>),
}

impl Session {
    fn detach_device_model(&self) {
        if let Self::V201(session) = self {
            session.detach_device_model();
        }
    }
    fn update(&self, snapshot: StationSnapshot) -> Result<(), StationCommandError> {
        match self {
            Self::V16(session) => session.update_committed(snapshot),
            Self::V201(session) => session.update_committed(snapshot),
        }
    }

    fn trigger_expectation(
        &self,
        command: &Command<Value>,
    ) -> Option<uob_application::TriggerExpectation> {
        match self {
            Self::V16(session) => session.trigger_expectation(command),
            Self::V201(session) => session.trigger_expectation(command),
        }
    }

    async fn context(
        &self,
        resource: ResourceRef,
    ) -> Result<Option<StationCommandContext>, StationCommandError> {
        match self {
            Self::V16(session) => session.context(resource).await,
            Self::V201(session) => session.context(resource).await,
        }
    }

    async fn dispatch(
        &self,
        command: Command<Value>,
    ) -> Result<CommandDispatchOutcome, StationCommandError> {
        match self {
            Self::V16(session) => session.dispatch(command).await,
            Self::V201(session) => session.dispatch(command).await,
        }
    }
}

/// An entry belongs to exactly one socket generation; removal never evicts a reconnect.
pub(super) struct LiveCommands {
    next: AtomicU64,
    sessions: RwLock<BTreeMap<StationId, (u64, Arc<Session>)>>,
}

pub(super) struct SnapshotCommit(Arc<Session>);
impl Drop for SnapshotCommit {
    fn drop(&mut self) {
        if let Session::V201(session) = self.0.as_ref() {
            session.abort_snapshot_commit();
        }
    }
}

impl LiveCommands {
    pub(super) fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
            sessions: RwLock::new(BTreeMap::new()),
        }
    }

    pub(super) fn attach_16(
        &self,
        station: StationId,
        session: Arc<v16::remote_control::RemoteControlSession>,
    ) -> u64 {
        self.attach(station, Arc::new(Session::V16(session)))
    }

    pub(super) fn attach_201(
        &self,
        station: StationId,
        session: v201::remote_control::RemoteControlSession,
        store: super::ChargingStore,
    ) -> u64 {
        let generation = self.next.fetch_add(1, Ordering::Relaxed);
        let session = Arc::new(session.with_device_model(Arc::new(store), generation));
        if let Some((_, old)) = self
            .sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(station, (generation, Arc::new(Session::V201(session))))
        {
            old.detach_device_model();
        }
        generation
    }

    fn attach(&self, station: StationId, session: Arc<Session>) -> u64 {
        let generation = self.next.fetch_add(1, Ordering::Relaxed);
        if let Some((_, old)) = self
            .sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(station, (generation, session))
        {
            old.detach_device_model();
        }
        generation
    }

    pub(super) fn update(
        &self,
        station: &StationId,
        generation: u64,
        snapshot: StationSnapshot,
    ) -> Result<(), StationCommandError> {
        let sessions = self
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((current, session)) = sessions.get(station)
            && *current == generation
        {
            return session.update(snapshot);
        }
        Ok(())
    }

    pub(super) fn begin_snapshot_commit(
        &self,
        station: &StationId,
        generation: u64,
    ) -> Result<Option<SnapshotCommit>, StationCommandError> {
        let sessions = self
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((current, session)) = sessions.get(station)
            && *current == generation
            && let Session::V201(port) = session.as_ref()
        {
            port.begin_snapshot_commit()?;
            return Ok(Some(SnapshotCommit(session.clone())));
        }
        Ok(None)
    }

    pub(super) fn remove(&self, station: &StationId, generation: u64) {
        let mut sessions = self
            .sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if sessions
            .get(station)
            .is_some_and(|(current, _)| *current == generation)
            && let Some((_, session)) = sessions.remove(station)
        {
            session.detach_device_model();
        }
    }

    fn session(&self, station: &StationId) -> Option<Arc<Session>> {
        self.sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(station)
            .map(|(_, session)| Arc::clone(session))
    }
}

impl StationCommandPort<Value> for LiveCommands {
    fn charging_profile_expectation(
        &self,
        command: &Command<Value>,
        generation: Option<u64>,
        now: uob_contracts::UtcTimestamp,
    ) -> Result<Option<uob_application::ProfileReservation201>, uob_contracts::CommandErrorCode>
    {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| uob_contracts::CommandErrorCode::PolicyRejected)?;
        let (current, session) = sessions
            .get(&command.resource.station_id)
            .ok_or(uob_contracts::CommandErrorCode::StationDisconnected)?;
        if Some(*current) != generation {
            return Err(uob_contracts::CommandErrorCode::StationDisconnected);
        }
        match session.as_ref() {
            Session::V201(session) => {
                session.charging_profile_expectation(command, generation, now)
            }
            Session::V16(session) => session.charging_profile_expectation(command, generation, now),
        }
    }
    fn dispatch_reserved_profile(
        &self,
        command: Command<Value>,
        expected: Option<u64>,
        reservation: uob_application::ProfileReservation201,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        let selected = self.sessions.read().ok().and_then(|sessions| {
            sessions
                .get(&command.resource.station_id)
                .filter(|(generation, _)| {
                    Some(*generation) == expected && *generation == reservation.generation
                })
                .map(|(_, session)| Arc::clone(session))
        });
        Box::pin(async move {
            let Some(session) = selected else {
                return Ok(disconnected());
            };
            let Some(context) = session.context(command.resource.clone()).await? else {
                return Ok(disconnected());
            };
            if !matches!(context.connectivity, Connectivity::Connected { connected_at, .. }
                if command.admitted_at >= connected_at)
                || self.session_generation(&command.resource) != expected
            {
                return Ok(disconnected());
            }
            match session.as_ref() {
                Session::V201(session) => {
                    session
                        .dispatch_reserved_profile(command, expected, reservation)
                        .await
                }
                Session::V16(_) => Ok(disconnected()),
            }
        })
    }
    fn context(
        &self,
        resource: ResourceRef,
    ) -> StationCommandFuture<'_, Option<StationCommandContext>> {
        let session = self.session(&resource.station_id);
        Box::pin(async move {
            match session {
                Some(session) => session.context(resource).await,
                None => Ok(None),
            }
        })
    }

    fn trigger_expectation(
        &self,
        command: &Command<Value>,
    ) -> Option<uob_application::TriggerExpectation> {
        self.session(&command.resource.station_id)?
            .trigger_expectation(command)
    }
    fn device_model_expectation(
        &self,
        command: &Command<Value>,
        generation: Option<u64>,
        now: uob_contracts::UtcTimestamp,
    ) -> Result<Option<uob_contracts::DeviceModelResult201>, uob_contracts::CommandErrorCode> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| uob_contracts::CommandErrorCode::PolicyRejected)?;
        let Some((current, session)) = sessions.get(&command.resource.station_id) else {
            return Ok(None);
        };
        if Some(*current) != generation {
            return Err(uob_contracts::CommandErrorCode::StationDisconnected);
        }
        match session.as_ref() {
            Session::V201(session) => session.device_model_expectation(command, generation, now),
            Session::V16(_) => Ok(None),
        }
    }

    fn session_generation(&self, resource: &ResourceRef) -> Option<u64> {
        self.sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&resource.station_id)
            .map(|(generation, _)| *generation)
    }

    fn dispatch_to_generation(
        &self,
        command: Command<Value>,
        expected: Option<u64>,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        let selected = self
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&command.resource.station_id)
            .filter(|(generation, _)| Some(*generation) == expected)
            .map(|(_, session)| Arc::clone(session));
        if matches!(&command.operation, uob_contracts::CommandOperation::Ocpp(operation)
            if operation.protocol == uob_contracts::ProtocolEdition::Ocpp201
                && ["GetVariables", "GetBaseReport", "GetReport"].contains(&operation.action.as_str()))
            && let Some(session) = selected.clone()
        {
            let dispatched = tokio::spawn(async move { session.dispatch(command).await });
            return Box::pin(async move {
                dispatched.await.map_err(|_| {
                    StationCommandError::new("device-model dispatch supervisor stopped")
                })?
            });
        }
        Box::pin(async move {
            let Some(session) = selected else {
                return Ok(disconnected());
            };
            let Some(context) = session.context(command.resource.clone()).await? else {
                return Ok(disconnected());
            };
            let Connectivity::Connected { connected_at, .. } = context.connectivity else {
                return Ok(disconnected());
            };
            if command.admitted_at < connected_at {
                return Ok(disconnected());
            }
            if self.session_generation(&command.resource) != expected {
                return Ok(disconnected());
            }
            session.dispatch(command).await
        })
    }

    fn dispatch(
        &self,
        command: Command<Value>,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        let generation = self.session_generation(&command.resource);
        self.dispatch_to_generation(command, generation)
    }
}

fn disconnected() -> CommandDispatchOutcome {
    CommandDispatchOutcome::NotTransmitted {
        error: uob_contracts::CommandError {
            code: uob_contracts::CommandErrorCode::StationDisconnected,
            detail: None,
        },
    }
}
