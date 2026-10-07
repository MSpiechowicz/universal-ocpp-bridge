//! Independent OCPP 2.0.1 station firmware model: L01 Secure and L02 Non-Secure Firmware
//! Update through one `UpdateFirmware` message. A station implements exactly one mode.
mod config;
mod model;
pub(crate) mod request;
pub(crate) mod transport;

use config::Settings;
pub use config::{CancelPolicy201, FirmwareConfig201, FirmwareMode201};
use model::{PrivateState, RebootState};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fmt, fmt::Write as _, sync::Arc, time::Duration};
use time::OffsetDateTime;
use zeroize::Zeroize;

use crate::firmware16::verify::TrustRoots;
use crate::reservation16::persistence::PrivateStorage;

/// One native status awaiting its CALL, always bound to the request's `requestId`.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FirmwareStatus201 {
    pub job: u64,
    pub status: String,
    pub request_id: i32,
}

/// Facts the station runtime observes outside the firmware model.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StationFacts201 {
    /// No transaction is active, so installation may begin (L01.FR.06).
    pub idle: bool,
    /// Socket generation on which a `BootNotification` was accepted, while it is current.
    pub boot_generation: Option<u64>,
}

/// One download attempt; the location never appears in debug output.
#[derive(Clone, Eq, PartialEq)]
pub struct DownloadTicket201 {
    pub job: u64,
    pub attempt: u32,
    pub simulated_failure: bool,
    pub(crate) location: String,
}

impl fmt::Debug for DownloadTicket201 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DownloadTicket201")
            .field("job", &self.job)
            .field("attempt", &self.attempt)
            .field("simulated_failure", &self.simulated_failure)
            .finish_non_exhaustive()
    }
}

impl Drop for DownloadTicket201 {
    fn drop(&mut self) {
        self.location.zeroize();
    }
}

/// Work the runtime performs for the model: start a download or reboot with a new version.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct FirmwareEffects201 {
    pub download: Option<DownloadTicket201>,
    pub reboot: Option<String>,
}

pub(crate) struct Model {
    state: PrivateState,
    storage: Option<PrivateStorage>,
    settings: Settings,
    roots: Option<TrustRoots>,
    unavailable: bool,
    reboot: RebootState,
    sending: bool,
    send_retry_at: Option<OffsetDateTime>,
}

#[derive(Clone)]
pub struct Firmware201Handle(Arc<Mutex<Model>>);
impl fmt::Debug for Firmware201Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Firmware201Handle(<private>)")
    }
}

impl Firmware201Handle {
    /// Open an exclusively owned, bounded, private firmware station state.
    /// # Errors
    /// Invalid configuration, unsafe files, corrupt data and failed durable writes fail closed.
    pub fn open(station: &str, config: &FirmwareConfig201) -> Result<Self, &'static str> {
        config.validate()?;
        let roots = config
            .manufacturer_root_file
            .as_deref()
            .map(crate::firmware16::read_roots)
            .transpose()?;
        let storage = PrivateStorage::open(&config.private_state_file)?;
        let mut state = storage
            .load()?
            .unwrap_or_else(|| PrivateState::empty(station, config.mode));
        state.validate(station, config.mode)?;
        let reboot = state.resume();
        storage.commit(&state)?;
        Ok(Self::new(state, Some(storage), config, roots, reboot))
    }

    /// Non-durable model for isolated native semantics tests, never a live transport fallback.
    /// # Errors
    /// Invalid configuration or manufacturer roots.
    pub fn in_memory(
        station: &str,
        config: &FirmwareConfig201,
        roots_pem: Option<&[u8]>,
    ) -> Result<Self, &'static str> {
        let mut checked = config.clone();
        checked.manufacturer_root_file = roots_pem.map(|_| String::new());
        checked.validate()?;
        let roots = roots_pem.map(TrustRoots::parse).transpose()?;
        let state = PrivateState::empty(station, config.mode);
        Ok(Self::new(state, None, config, roots, RebootState::Idle))
    }

    fn new(
        state: PrivateState,
        storage: Option<PrivateStorage>,
        config: &FirmwareConfig201,
        roots: Option<TrustRoots>,
        reboot: RebootState,
    ) -> Self {
        Self(Arc::new(Mutex::new(Model {
            state,
            storage,
            settings: Settings::from(config),
            roots,
            unavailable: false,
            reboot,
            sending: false,
            send_retry_at: None,
        })))
    }

    #[must_use]
    pub fn mode(&self) -> FirmwareMode201 {
        self.0.lock().settings.mode
    }

    /// Reply to a native `UpdateFirmware` CALL: the CALLRESULT payload, or
    /// `{"callError": <code>}` for a CALLERROR.
    #[must_use]
    pub fn handle_call(&self, payload: &Value, now: OffsetDateTime) -> Value {
        let mut model = self.0.lock();
        request::parse(payload)
            .and_then(|request| model.update(&request, now))
            .unwrap_or_else(|code| json!({"callError": code}))
    }

    /// Advance time- and fact-driven transitions.
    /// # Errors
    /// An unavailable or undurable model fails closed without effects.
    pub fn advance(
        &self,
        now: OffsetDateTime,
        facts: StationFacts201,
    ) -> Result<FirmwareEffects201, &'static str> {
        let mut model = self.0.lock();
        let mut next = model.state.clone();
        let mut reboot = model.reboot;
        let effects = next.advance(now, facts, &model.settings, &mut reboot)?;
        if next != model.state {
            model.commit(next)?;
        }
        model.reboot = reboot;
        Ok(effects)
    }

    /// Record one finished download attempt, verifying a secure image over the whole file.
    /// # Errors
    /// Failed private durability; a stale ticket is ignored.
    pub fn download_finished(
        &self,
        ticket: &DownloadTicket201,
        image: Option<&[u8]>,
        now: OffsetDateTime,
    ) -> Result<(), &'static str> {
        let mut model = self.0.lock();
        let Some(job) = model.state.job.as_ref().filter(|job| {
            job.id == ticket.job
                && job.attempts == ticket.attempt
                && job.phase == model::Phase::Downloading
        }) else {
            return Ok(());
        };
        let outcome = image.map(|image| {
            let hex =
                Sha256::digest(image)
                    .iter()
                    .fold(String::with_capacity(64), |mut hex, byte| {
                        let _ = write!(hex, "{byte:02x}");
                        hex
                    });
            let valid = match (&model.roots, &job.signing_certificate, &job.signature) {
                (Some(roots), Some(certificate), Some(signature)) => {
                    Some(roots.signature_valid_201(certificate, signature, image, now))
                }
                // L01 scenario 5: a missing signature is an invalid signature.
                (Some(_), _, _) => Some(false),
                _ => None,
            };
            (hex, valid)
        });
        let mut next = model.state.clone();
        next.downloaded(outcome, now)?;
        model.commit(next)
    }

    /// Claim the oldest pending status for one in-flight CALL.
    #[must_use]
    pub fn next_status(&self, now: OffsetDateTime) -> Option<FirmwareStatus201> {
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
        status: &FirmwareStatus201,
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

    /// Payload for `TriggerMessage(FirmwareStatusNotification)`: `Idle` after `Installed` or
    /// before any report (L01.FR.25), otherwise the last sent status and requestId (L01.FR.26).
    #[must_use]
    pub fn trigger_payload(&self) -> Value {
        match &self.0.lock().state.last_sent {
            Some((status, request_id)) if status != "Installed" => {
                json!({"status": status, "requestId": request_id})
            }
            _ => json!({"status": "Idle"}),
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
            timeout: model.settings.download_timeout,
        }
    }

    /// Safe workflow metadata only: no location, certificate, signature or digest.
    #[must_use]
    pub fn snapshot(&self) -> Value {
        let model = self.0.lock();
        let job = model.state.job.as_ref();
        let mut snapshot = json!({
            "stateAvailable": !model.unavailable,
            "mode": model.settings.mode,
            "active": model.state.active(),
            "statuses": job.map_or_else(Vec::new, |job| job.sent.clone()),
            "pendingStatuses": model.state.outbox.len(),
            "reboots": model.state.reboots,
            "cancelled": model.state.cancelled,
            "revision": model.state.revision,
        });
        if let Some(job) = job {
            snapshot["requestId"] = job.request_id.into();
        }
        if let Some(last) = job.and_then(|job| job.sent.last()) {
            snapshot["lastStatus"] = last.clone().into();
        }
        if let Some(version) = &model.state.installed_version {
            snapshot["installedVersion"] = version.clone().into();
        }
        snapshot
    }
}

impl Model {
    fn commit(&mut self, mut next: PrivateState) -> Result<(), &'static str> {
        if self.unavailable {
            return Err("firmware_state_unavailable");
        }
        next.revision = self
            .state
            .revision
            .checked_add(1)
            .ok_or("firmware_revision_capacity")?;
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

    fn update(
        &mut self,
        request: &request::FirmwareRequest201,
        now: OffsetDateTime,
    ) -> Result<Value, &'static str> {
        if self.settings.reject_updates {
            return Ok(json!({"status": "Rejected"}));
        }
        // L01.FR.21/22: a secure station validates the certificate before accepting.
        if self.settings.mode == FirmwareMode201::Secure {
            let trusted = self.roots.as_ref().is_some_and(|roots| {
                request
                    .signing_certificate
                    .as_deref()
                    .is_some_and(|certificate| roots.certificate_trusted(certificate, now))
            });
            if !trusted {
                return Ok(json!({"status": "InvalidCertificate"}));
            }
        }
        let mut next = self.state.clone();
        let ongoing = next.cancel_active(self.settings.cancel_policy);
        if ongoing == Some("Rejected") {
            return Ok(
                json!({"status": "Rejected", "statusInfo": {"reasonCode": "UnableToCancel"}}),
            );
        }
        next.accept(request, now)
            .and_then(|()| self.commit(next))
            .map_err(|_| "InternalError")?;
        Ok(json!({"status": ongoing.unwrap_or("Accepted")}))
    }
}
