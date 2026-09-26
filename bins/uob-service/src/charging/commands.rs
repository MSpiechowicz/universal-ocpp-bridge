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
    fn update(&self, snapshot: StationSnapshot) -> Result<(), StationCommandError> {
        match self {
            Self::V16(session) => session.update_committed(snapshot),
            Self::V201(session) => session.update_committed(snapshot),
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
        session: Arc<v201::remote_control::RemoteControlSession>,
    ) -> u64 {
        self.attach(station, Arc::new(Session::V201(session)))
    }

    fn attach(&self, station: StationId, session: Arc<Session>) -> u64 {
        let generation = self.next.fetch_add(1, Ordering::Relaxed);
        self.sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(station, (generation, session));
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

    pub(super) fn remove(&self, station: &StationId, generation: u64) {
        let mut sessions = self
            .sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if sessions
            .get(station)
            .is_some_and(|(current, _)| *current == generation)
        {
            sessions.remove(station);
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
