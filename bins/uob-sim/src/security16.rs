//! Independent OCPP 1.6 station security model: Security Whitepaper Edition 4 A04
//! (`SecurityEventNotification` with a durable, guaranteed-delivery outbox) and §5.5/§5.6
//! (`ExtendedTriggerMessage`). Certificates, signing and security-profile changes are not modelled.
mod config;
mod model;
mod outbox;
pub(crate) mod transport;

pub use config::{ExtendedTriggerConfig, SecurityConfig};
pub use model::{TECH_INFO_LIMIT, TYPE_LIMIT, validate_event};
pub use outbox::SecurityEvent;

use config::Settings;
use model::ExtendedMessage;
use outbox::PrivateState;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{collections::VecDeque, fmt, sync::Arc, time::Duration};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// How one delivery attempt of the claimed event ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delivery {
    /// `SecurityEventNotification.conf` arrived: the event leaves the outbox.
    Acknowledged,
    /// A non-transient CALLERROR: the event is dropped and counted as rejected.
    Rejected,
    /// No answer (closed socket, timeout) or a transient CALLERROR: the event stays queued.
    Failed,
}

/// Messages a triggered station sends once its `ExtendedTriggerMessage.conf` was written.
pub(crate) struct Staged {
    pub(crate) generation: u64,
    released: bool,
    pub(crate) messages: Vec<(ExtendedMessage, Value)>,
}

pub(crate) struct Model {
    state: PrivateState,
    storage: Option<crate::reservation16::persistence::PrivateStorage>,
    settings: Settings,
    connectors: Vec<u16>,
    unavailable: bool,
    sending: bool,
    send_retry_at: Option<OffsetDateTime>,
    staged: VecDeque<Staged>,
    reply_delay: Option<Duration>,
    triggers: u64,
    last_trigger: Option<String>,
}

#[derive(Clone)]
pub struct SecurityHandle(pub(crate) Arc<Mutex<Model>>);
impl fmt::Debug for SecurityHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecurityHandle(<private>)")
    }
}

/// Opens the configured station model for a native OCPP 1.6 client.
pub(crate) fn open(
    config: &crate::SimulatorClientConfig,
) -> Result<Option<SecurityHandle>, crate::SimulatorClientError> {
    config
        .security16
        .as_ref()
        .map(|(station, options)| {
            SecurityHandle::open(station, &config.connectors, options)
                .map_err(|code| crate::SimulatorClientError::Protocol(code.to_owned()))
        })
        .transpose()
}

fn timestamp(value: OffsetDateTime) -> Result<String, &'static str> {
    // Millisecond precision keeps distinct events from sharing one timestamp.
    value
        .replace_nanosecond(value.nanosecond() / 1_000_000 * 1_000_000)
        .map_err(|_| "security_timestamp")?
        .format(&Rfc3339)
        .map_err(|_| "security_timestamp")
}

impl SecurityHandle {
    /// Open an exclusively owned, bounded, private security-event outbox.
    /// # Errors
    /// Invalid configuration, unsafe files, corrupt data and failed durable writes fail closed.
    pub fn open(
        station: &str,
        connectors: &[u16],
        config: &SecurityConfig,
    ) -> Result<Self, &'static str> {
        config.validate()?;
        let storage =
            crate::reservation16::persistence::PrivateStorage::open(&config.private_state_file)?;
        let state = storage
            .load()?
            .unwrap_or_else(|| PrivateState::empty(station));
        state.validate(station)?;
        storage.commit(&state)?;
        Ok(Self::new(state, Some(storage), connectors, config))
    }

    /// Non-durable model for isolated native semantics tests, never a live transport fallback.
    /// # Errors
    /// Invalid configuration.
    pub fn in_memory(
        station: &str,
        connectors: &[u16],
        config: &SecurityConfig,
    ) -> Result<Self, &'static str> {
        config.validate()?;
        Ok(Self::new(
            PrivateState::empty(station),
            None,
            connectors,
            config,
        ))
    }

    fn new(
        state: PrivateState,
        storage: Option<crate::reservation16::persistence::PrivateStorage>,
        connectors: &[u16],
        config: &SecurityConfig,
    ) -> Self {
        Self(Arc::new(Mutex::new(Model {
            state,
            storage,
            settings: Settings::from(config),
            connectors: connectors.to_vec(),
            unavailable: false,
            sending: false,
            send_retry_at: None,
            staged: VecDeque::new(),
            reply_delay: None,
            triggers: 0,
            last_trigger: None,
        })))
    }

    /// Queue one security event durably; it is sent once the connection is usable and stays
    /// queued until acknowledged. An omitted timestamp is the current time.
    /// # Errors
    /// A schema bound violation (`security_event_*`), a full outbox or failed private durability.
    pub fn emit(
        &self,
        kind: &str,
        event_time: Option<&str>,
        tech_info: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<u64, &'static str> {
        let event_time = match event_time {
            Some(text) => text.to_owned(),
            None => timestamp(now)?,
        };
        let mut model = self.0.lock();
        let mut next = model.state.clone();
        let id = next.queue(kind, &event_time, tech_info)?;
        model.commit(next)?;
        Ok(id)
    }

    /// Claim the oldest queued event for one in-flight CALL.
    #[must_use]
    pub fn next_event(&self, now: OffsetDateTime) -> Option<SecurityEvent> {
        let mut model = self.0.lock();
        if model.unavailable
            || model.sending
            || model.send_retry_at.is_some_and(|retry| retry > now)
        {
            return None;
        }
        let event = model.state.outbox.front().cloned()?;
        model.sending = true;
        Some(event)
    }

    /// Release a claimed event according to how its delivery attempt ended.
    /// # Errors
    /// Failed private durability keeps the event for at-least-once delivery.
    pub fn event_finished(
        &self,
        event: &SecurityEvent,
        outcome: Delivery,
        now: OffsetDateTime,
    ) -> Result<(), &'static str> {
        let mut model = self.0.lock();
        model.sending = false;
        if outcome == Delivery::Failed {
            model.send_retry_at = Some(now + time::Duration::seconds(1));
            return Ok(());
        }
        model.send_retry_at = None;
        let mut next = model.state.clone();
        if outcome == Delivery::Acknowledged {
            let duplicate = model.settings.duplicate_acknowledged;
            next.acknowledge(event, duplicate)?;
        } else {
            next.reject(event);
        }
        model.commit(next)
    }

    /// Delivery before the CSMS accepted this connection's Boot (fault knob).
    #[must_use]
    pub fn send_before_accepted(&self) -> bool {
        self.0.lock().settings.send_before_accepted
    }

    /// Safe workflow metadata only: counters and the last event type, never `techInfo`.
    #[must_use]
    pub fn snapshot(&self) -> Value {
        let model = self.0.lock();
        let mut snapshot = json!({
            "stateAvailable": !model.unavailable,
            "pendingEvents": model.state.outbox.len(),
            "eventsEmitted": model.state.emitted,
            "eventsAcknowledged": model.state.acknowledged,
            "eventsRejected": model.state.rejected,
            "duplicatesSent": model.state.duplicates_sent,
            "triggersReceived": model.triggers,
            "revision": model.state.revision,
        });
        if let Some(kind) = &model.state.last_acknowledged_type {
            snapshot["lastAcknowledgedType"] = kind.clone().into();
        }
        if let Some(trigger) = &model.last_trigger {
            snapshot["lastTrigger"] = trigger.clone().into();
        }
        snapshot
    }
}

impl Model {
    fn commit(&mut self, mut next: PrivateState) -> Result<(), &'static str> {
        if self.unavailable {
            return Err("security_state_unavailable");
        }
        next.revision = self
            .state
            .revision
            .checked_add(1)
            .ok_or("security_revision_capacity")?;
        if let Some(storage) = &self.storage
            && let Err(error) = storage.commit(&next)
        {
            if error == "reservation_commit_uncertain" {
                self.state = next;
                self.unavailable = true;
            }
            return Err(error);
        }
        self.state = next;
        Ok(())
    }

    pub(crate) fn settings(&self) -> &Settings {
        &self.settings
    }

    pub(crate) fn connectors(&self) -> &[u16] {
        &self.connectors
    }

    /// Record one answered `ExtendedTriggerMessage`; an accepted one with messages is staged until
    /// its reply was written, and its reply may be held back.
    pub(crate) fn note_trigger(
        &mut self,
        summary: String,
        staged: Option<Staged>,
        delay: Duration,
    ) {
        self.triggers = self.triggers.saturating_add(1);
        self.last_trigger = Some(summary);
        self.reply_delay = (!delay.is_zero()).then_some(delay);
        if let Some(staged) = staged {
            self.staged.push_back(staged);
        }
    }

    pub(crate) fn take_reply_delay(&mut self) -> Option<Duration> {
        self.reply_delay.take()
    }

    pub(crate) fn release_staged(&mut self) {
        for staged in &mut self.staged {
            staged.released = true;
        }
    }

    /// The next released trigger of this socket generation; older generations are dropped, since
    /// nothing is resent after a reconnect.
    pub(crate) fn claim_staged(&mut self, generation: u64) -> Option<Staged> {
        self.staged.retain(|staged| staged.generation >= generation);
        let index = self
            .staged
            .iter()
            .position(|staged| staged.released && staged.generation == generation)?;
        self.staged.remove(index)
    }
}

impl Staged {
    pub(crate) fn new(generation: u64, messages: Vec<(ExtendedMessage, Value)>) -> Self {
        Self {
            generation,
            released: false,
            messages,
        }
    }
}
