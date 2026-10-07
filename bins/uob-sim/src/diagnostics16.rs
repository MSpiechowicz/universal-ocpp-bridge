//! Independent OCPP 1.6 station log-upload model: Edition 2 §4.4/§5.9 (`GetDiagnostics` and
//! `DiagnosticsStatusNotification`) and Security Whitepaper Edition 4 N01 (`GetLog` and
//! `LogStatusNotification`). A station may implement either family or both.
mod config;
mod model;
pub(crate) mod request;
pub(crate) mod transport;

use config::Settings;
pub use config::{DiagnosticsConfig, LogCancelPolicy, LogFailureStatus};
use model::PrivateState;
use parking_lot::Mutex;
pub use request::LogKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fmt, sync::Arc, time::Duration};
use time::OffsetDateTime;
use zeroize::Zeroize;

/// One native status awaiting its CALL; `kind` selects the legacy or whitepaper message.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LogStatus {
    pub job: u64,
    pub kind: LogKind,
    pub status: String,
    pub request_id: Option<i32>,
}

/// One upload attempt; the location never appears in debug output.
#[derive(Clone, Eq, PartialEq)]
pub struct UploadTicket {
    pub job: u64,
    pub attempt: u32,
    pub simulated_failure: bool,
    pub file_name: String,
    pub(crate) location: String,
    pub(crate) content: Vec<u8>,
}

impl fmt::Debug for UploadTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UploadTicket")
            .field("job", &self.job)
            .field("attempt", &self.attempt)
            .field("simulated_failure", &self.simulated_failure)
            .field("file_name", &self.file_name)
            .finish_non_exhaustive()
    }
}

impl Drop for UploadTicket {
    fn drop(&mut self) {
        self.location.zeroize();
    }
}

impl UploadTicket {
    /// Generated log bytes this attempt uploads.
    #[must_use]
    pub fn content(&self) -> &[u8] {
        &self.content
    }
}

pub(crate) struct Model {
    state: PrivateState,
    storage: Option<crate::reservation16::persistence::PrivateStorage>,
    settings: Settings,
    unavailable: bool,
    sending: bool,
    send_retry_at: Option<OffsetDateTime>,
}

#[derive(Clone)]
pub struct DiagnosticsHandle(Arc<Mutex<Model>>);
impl fmt::Debug for DiagnosticsHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DiagnosticsHandle(<private>)")
    }
}

impl DiagnosticsHandle {
    /// Open an exclusively owned, bounded, private log-upload station state.
    /// # Errors
    /// Invalid configuration, unsafe files, corrupt data and failed durable writes fail closed.
    pub fn open(station: &str, config: &DiagnosticsConfig) -> Result<Self, &'static str> {
        config.validate()?;
        let storage =
            crate::reservation16::persistence::PrivateStorage::open(&config.private_state_file)?;
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
    pub fn in_memory(station: &str, config: &DiagnosticsConfig) -> Result<Self, &'static str> {
        config.validate()?;
        Ok(Self::new(PrivateState::empty(station), None, config))
    }

    fn new(
        state: PrivateState,
        storage: Option<crate::reservation16::persistence::PrivateStorage>,
        config: &DiagnosticsConfig,
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

    /// Reply to a native `GetDiagnostics` or `GetLog` CALL.
    ///
    /// The result is the CALLRESULT payload, or `{"callError": <code>}` for a CALLERROR.
    #[must_use]
    pub fn handle_call(&self, action: &str, payload: &Value) -> Value {
        let mut model = self.0.lock();
        let reply = match action {
            "GetDiagnostics" if model.settings.legacy => {
                request::diagnostics(payload).and_then(|request| model.diagnostics(&request))
            }
            "GetLog" if model.settings.security_log => {
                request::get_log(payload).and_then(|request| model.get_log(&request))
            }
            // An unsupported family keeps the client library's reply.
            _ => Err("NotImplemented"),
        };
        reply.unwrap_or_else(|code| json!({"callError": code}))
    }

    /// Advance time-driven transitions.
    /// # Errors
    /// An unavailable or undurable model fails closed without effects.
    pub fn advance(&self, now: OffsetDateTime) -> Result<Option<UploadTicket>, &'static str> {
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
        ticket: &UploadTicket,
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
    pub fn next_status(&self, now: OffsetDateTime) -> Option<LogStatus> {
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
        status: &LogStatus,
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

    /// Status for `TriggerMessage(DiagnosticsStatusNotification)`: `Idle` only when not busy
    /// uploading diagnostics (§4.4), otherwise `Uploading`. A whitepaper `GetLog` upload is
    /// reported only through `LogStatusNotification`.
    #[must_use]
    pub fn trigger_status(&self) -> &'static str {
        let model = self.0.lock();
        if model.state.active()
            && model
                .state
                .job
                .as_ref()
                .is_some_and(|job| job.kind.legacy())
        {
            "Uploading"
        } else {
            "Idle"
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
            "legacy": model.settings.legacy,
            "securityLog": model.settings.security_log,
            "active": model.state.active(),
            "statuses": job.map_or_else(Vec::new, |job| job.sent.clone()),
            "pendingStatuses": model.state.outbox.len(),
            "uploads": model.state.uploads,
            "cancelled": model.state.cancelled,
            "revision": model.state.revision,
        });
        if let Some(job) = job {
            snapshot["kind"] = json!(job.kind);
            snapshot["fileName"] = job.file_name.clone().into();
            snapshot["attempts"] = job.attempts.into();
            if let Some(id) = job.request_id {
                snapshot["requestId"] = id.into();
            }
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

    /// §5.9: a station with nothing to upload, or already uploading, answers without a file
    /// name and starts nothing (1.6 defines no cancellation for `GetDiagnostics`).
    fn diagnostics(&mut self, request: &request::LogRequest) -> Result<Value, &'static str> {
        if self.settings.no_diagnostics || self.state.active() {
            return Ok(json!({}));
        }
        let mut next = self.state.clone();
        let file_name = next.accept(request).map_err(|_| "InternalError")?;
        self.commit(next).map_err(|_| "InternalError")?;
        Ok(json!({ "fileName": file_name }))
    }

    /// N01.FR.01 with N01.FR.11 for a request arriving during an ongoing upload.
    fn get_log(&mut self, request: &request::LogRequest) -> Result<Value, &'static str> {
        if self.settings.reject_get_log {
            return Ok(json!({"status": "Rejected"}));
        }
        let mut next = self.state.clone();
        let ongoing = next.cancel_active(self.settings.cancel_policy);
        if ongoing == Some("Rejected") {
            return Ok(json!({"status": "Rejected"}));
        }
        let file_name = next.accept(request).map_err(|_| "InternalError")?;
        self.commit(next).map_err(|_| "InternalError")?;
        Ok(json!({"status": ongoing.unwrap_or("Accepted"), "filename": file_name}))
    }
}
