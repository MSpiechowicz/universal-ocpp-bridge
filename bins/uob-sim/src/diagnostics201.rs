//! Independent OCPP 2.0.1 station log-upload model: use case N01 (`GetLog` and
//! `LogStatusNotification`), written from the pinned Edition 4 text and JSON schemas.
mod config;
mod model;
pub(crate) mod request;
pub(crate) mod transport;

use config::Settings;
pub use config::{Diagnostics201Config, LogCancelPolicy201, LogFailureStatus201};
use model::PrivateState;
use parking_lot::Mutex;
pub use request::LogKind201;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fmt, sync::Arc, time::Duration};
use time::OffsetDateTime;
use zeroize::Zeroize;

use crate::reservation16::persistence::PrivateStorage;

/// One native status awaiting its CALL, always bound to the request's `requestId` (N01.FR.07).
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LogStatus201 {
    pub job: u64,
    pub status: String,
    pub request_id: i32,
}

/// One upload attempt; the location never appears in debug output.
#[derive(Clone, Eq, PartialEq)]
pub struct UploadTicket201 {
    pub job: u64,
    pub attempt: u32,
    pub simulated_failure: bool,
    pub file_name: String,
    pub(crate) location: String,
    pub(crate) content: Vec<u8>,
}

impl fmt::Debug for UploadTicket201 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UploadTicket201")
            .field("job", &self.job)
            .field("attempt", &self.attempt)
            .field("simulated_failure", &self.simulated_failure)
            .field("file_name", &self.file_name)
            .finish_non_exhaustive()
    }
}

impl Drop for UploadTicket201 {
    fn drop(&mut self) {
        self.location.zeroize();
    }
}

impl UploadTicket201 {
    /// Generated log bytes this attempt uploads.
    #[must_use]
    pub fn content(&self) -> &[u8] {
        &self.content
    }
}

pub(crate) struct Model {
    state: PrivateState,
    storage: Option<PrivateStorage>,
    settings: Settings,
    unavailable: bool,
    sending: bool,
    send_retry_at: Option<OffsetDateTime>,
}

#[derive(Clone)]
pub struct Diagnostics201Handle(Arc<Mutex<Model>>);
impl fmt::Debug for Diagnostics201Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Diagnostics201Handle(<private>)")
    }
}

impl Diagnostics201Handle {
    /// Open an exclusively owned, bounded, private log-upload station state.
    /// # Errors
    /// Invalid configuration, unsafe files, corrupt data and failed durable writes fail closed.
    pub fn open(station: &str, config: &Diagnostics201Config) -> Result<Self, &'static str> {
        config.validate()?;
        let storage = PrivateStorage::open(&config.private_state_file)?;
        let mut state = storage
            .load()?
            .unwrap_or_else(|| PrivateState::empty(station));
        state.validate(station)?;
        state.resume();
        storage.commit(&state)?;
        Ok(Self::new(state, Some(storage), config))
    }

    /// Non-durable model for isolated native semantics tests, never a live transport fallback.
    /// # Errors
    /// Invalid configuration.
    pub fn in_memory(station: &str, config: &Diagnostics201Config) -> Result<Self, &'static str> {
        config.validate()?;
        Ok(Self::new(PrivateState::empty(station), None, config))
    }

    fn new(
        state: PrivateState,
        storage: Option<PrivateStorage>,
        config: &Diagnostics201Config,
    ) -> Self {
        Self(Arc::new(Mutex::new(Model {
            state,
            storage,
            settings: Settings::from(config),
            unavailable: false,
            sending: false,
            send_retry_at: None,
        })))
    }

    /// Reply to a native `GetLog` CALL: the CALLRESULT payload, or `{"callError": <code>}` for
    /// a CALLERROR.
    #[must_use]
    pub fn handle_call(&self, payload: &Value) -> Value {
        let mut model = self.0.lock();
        request::parse(payload)
            .and_then(|request| model.get_log(&request))
            .unwrap_or_else(|code| json!({"callError": code}))
    }

    /// Advance time-driven transitions.
    /// # Errors
    /// An unavailable or undurable model fails closed without effects.
    pub fn advance(&self, now: OffsetDateTime) -> Result<Option<UploadTicket201>, &'static str> {
        let mut model = self.0.lock();
        let mut next = model.state.clone();
        let ticket = next.advance(now, &model.settings)?;
        if next != model.state {
            model.commit(next)?;
        }
        Ok(ticket)
    }

    /// Record one finished upload attempt.
    /// # Errors
    /// Failed private durability; a stale ticket (cancelled or superseded job) is ignored.
    pub fn upload_finished(
        &self,
        ticket: &UploadTicket201,
        success: bool,
        now: OffsetDateTime,
    ) -> Result<(), &'static str> {
        let mut model = self.0.lock();
        if !model.state.job.as_ref().is_some_and(|job| {
            job.id == ticket.job
                && job.attempts == ticket.attempt
                && job.phase == model::Phase::Uploading
        }) {
            return Ok(());
        }
        // A simulated failure never counts as stored, whatever the transfer reported.
        let success = success && !ticket.simulated_failure;
        let mut next = model.state.clone();
        next.uploaded(success, now, &model.settings)?;
        model.commit(next)
    }

    /// Claim the oldest pending status for one in-flight CALL.
    #[must_use]
    pub fn next_status(&self, now: OffsetDateTime) -> Option<LogStatus201> {
        let mut model = self.0.lock();
        if model.unavailable
            || model.sending
            || model.send_retry_at.is_some_and(|retry| retry > now)
        {
            return None;
        }
        let status = model.state.outbox.front().cloned()?;
        model.sending = true;
        Some(status)
    }

    /// Release a claimed status; a delivered one leaves the durable outbox.
    /// # Errors
    /// Failed private durability keeps the status for at-least-once delivery.
    pub fn status_finished(
        &self,
        status: &LogStatus201,
        delivered: bool,
        now: OffsetDateTime,
    ) -> Result<(), &'static str> {
        let mut model = self.0.lock();
        model.sending = false;
        if !delivered {
            model.send_retry_at = Some(now + time::Duration::seconds(1));
            return Ok(());
        }
        model.send_retry_at = None;
        if model.state.outbox.front() != Some(status) {
            return Ok(());
        }
        let mut next = model.state.clone();
        next.outbox.pop_front();
        next.record_sent(status);
        model.commit(next)
    }

    /// Payload for `TriggerMessage(LogStatusNotification)` (N01.FR.13): `Uploading` with the
    /// ongoing request's identity while an upload is ongoing, otherwise `Idle` without one.
    #[must_use]
    pub fn trigger_payload(&self) -> Value {
        let model = self.0.lock();
        match model.state.job.as_ref().filter(|_| model.state.active()) {
            Some(job) => json!({"status": "Uploading", "requestId": job.request_id}),
            None => json!({"status": "Idle"}),
        }
    }

    #[must_use]
    pub fn status_delay(&self) -> Duration {
        self.0.lock().settings.status_delay
    }

    pub(crate) fn transfer_policy(&self) -> crate::artifact_transfer::TransferPolicy {
        let model = self.0.lock();
        crate::artifact_transfer::TransferPolicy {
            maximum_bytes: model.settings.maximum_bytes,
            timeout: model.settings.upload_timeout,
        }
    }

    /// Safe workflow metadata only: never the upload location or log content.
    #[must_use]
    pub fn snapshot(&self) -> Value {
        let model = self.0.lock();
        let job = model.state.job.as_ref();
        let mut snapshot = json!({
            "stateAvailable": !model.unavailable,
            "active": model.state.active(),
            "statuses": job.map_or_else(Vec::new, |job| job.sent.clone()),
            "pendingStatuses": model.state.outbox.len(),
            "uploads": model.state.uploads,
            "cancelled": model.state.cancelled,
            "revision": model.state.revision,
        });
        if let Some(job) = job {
            snapshot["kind"] = json!(job.kind);
            snapshot["requestId"] = job.request_id.into();
            snapshot["fileName"] = job.file_name.clone().into();
            snapshot["attempts"] = job.attempts.into();
            if let Some(last) = job.sent.last() {
                snapshot["lastStatus"] = last.clone().into();
            }
        }
        snapshot
    }
}

impl Model {
    fn commit(&mut self, mut next: PrivateState) -> Result<(), &'static str> {
        if self.unavailable {
            return Err("diagnostics_state_unavailable");
        }
        next.revision = self
            .state
            .revision
            .checked_add(1)
            .ok_or("diagnostics_revision_capacity")?;
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

    /// N01.FR.01/05 with N01.FR.12 for a request arriving during an ongoing upload.
    fn get_log(&mut self, request: &request::LogRequest201) -> Result<Value, &'static str> {
        if self.settings.reject_get_log {
            // N01.FR.05: the requested log information is not available.
            return Ok(
                json!({"status": "Rejected", "statusInfo": {"reasonCode": "NoLogAvailable"}}),
            );
        }
        let mut next = self.state.clone();
        let ongoing = next
            .cancel_active(self.settings.cancel_policy)
            .map_err(|_| "InternalError")?;
        if ongoing == Some("Rejected") {
            return Ok(
                json!({"status": "Rejected", "statusInfo": {"reasonCode": "UnableToCancel"}}),
            );
        }
        let file_name = next.accept(request).map_err(|_| "InternalError")?;
        self.commit(next).map_err(|_| "InternalError")?;
        Ok(json!({"status": ongoing.unwrap_or("Accepted"), "filename": file_name}))
    }
}
