//! Durable OCPP 2.0.1 station firmware state and its native transitions (L01, L02, Figure 116).
use super::{
    CancelPolicy201, FirmwareEffects201, FirmwareMode201, FirmwareStatus201, StationFacts201,
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use zeroize::Zeroize;

const HISTORY_LIMIT: usize = 32;
const OUTBOX_LIMIT: usize = 64;
const CANCELLED_LIMIT: usize = 16;
const RETRY_CAP_SECONDS: u32 = 60;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Scheduled,
    DownloadReady,
    Downloading,
    RetryWait,
    Downloaded,
    InstallScheduled,
    AwaitIdle,
    Rebooting,
    Finished,
}

#[derive(Clone, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Job {
    pub(super) id: u64,
    pub(super) request_id: i32,
    pub(super) location: String,
    pub(super) retrieve_at: String,
    pub(super) install_at: Option<String>,
    pub(super) retries: u32,
    pub(super) retry_interval: u32,
    pub(super) signing_certificate: Option<String>,
    pub(super) signature: Option<String>,
    pub(super) phase: Phase,
    pub(super) attempts: u32,
    pub(super) retry_at: Option<String>,
    pub(super) sha256: Option<String>,
    pub(super) signature_valid: Option<bool>,
    pub(super) sent: Vec<String>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.location.zeroize();
        if let Some(value) = &mut self.signing_certificate {
            value.zeroize();
        }
        if let Some(value) = &mut self.signature {
            value.zeroize();
        }
    }
}

#[derive(Clone, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct PrivateState {
    pub(super) format_version: u16,
    pub(super) station: String,
    pub(super) mode: FirmwareMode201,
    pub(super) jobs: u64,
    pub(super) job: Option<Job>,
    pub(super) outbox: VecDeque<FirmwareStatus201>,
    /// Station-wide last delivered status and its requestId (L01.FR.25/26).
    pub(super) last_sent: Option<(String, i32)>,
    pub(super) cancelled: Vec<i32>,
    pub(super) installed_version: Option<String>,
    pub(super) reboots: u64,
    pub(super) revision: u64,
}

/// Process-local reboot progress; a reload while rebooting counts as the reboot itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RebootState {
    Idle,
    Pending,
    Requested(u64),
    Restarted,
}

impl PrivateState {
    pub(super) fn empty(station: &str, mode: FirmwareMode201) -> Self {
        Self {
            format_version: 1,
            station: station.to_owned(),
            mode,
            jobs: 0,
            job: None,
            outbox: VecDeque::new(),
            last_sent: None,
            cancelled: Vec::new(),
            installed_version: None,
            reboots: 0,
            revision: 0,
        }
    }

    pub(super) fn validate(
        &self,
        station: &str,
        mode: FirmwareMode201,
    ) -> Result<(), &'static str> {
        let job_valid = self.job.as_ref().is_none_or(|job| {
            job.id != 0
                && job.id <= self.jobs
                && (mode == FirmwareMode201::Secure || job.signing_certificate.is_none())
                && parse(&job.retrieve_at).is_some()
                && job
                    .install_at
                    .as_deref()
                    .is_none_or(|at| parse(at).is_some())
                && job.retry_at.as_deref().is_none_or(|at| parse(at).is_some())
                && job.sent.len() <= HISTORY_LIMIT
        });
        if self.format_version != 1
            || self.station != station
            || self.mode != mode
            || !job_valid
            || self.outbox.len() > OUTBOX_LIMIT
            || self.cancelled.len() > CANCELLED_LIMIT
        {
            return Err("firmware_state_invalid");
        }
        Ok(())
    }

    /// A process restart loses any in-flight download, so that attempt starts again.
    pub(super) fn resume(&mut self) -> RebootState {
        match self.job.as_mut() {
            Some(job) if job.phase == Phase::Downloading => {
                job.phase = Phase::DownloadReady;
                job.attempts = job.attempts.saturating_sub(1);
                RebootState::Idle
            }
            Some(job) if job.phase == Phase::Rebooting => RebootState::Restarted,
            _ => RebootState::Idle,
        }
    }

    pub(super) fn active(&self) -> bool {
        self.job
            .as_ref()
            .is_some_and(|job| job.phase != Phase::Finished)
    }

    fn job_mut(&mut self) -> Result<&mut Job, &'static str> {
        self.job.as_mut().ok_or("firmware_job_missing")
    }

    /// L01.FR.01/10: every new state is reported with the request's own requestId.
    fn queue(&mut self, status: &'static str) -> Result<(), &'static str> {
        let job = self.job.as_ref().ok_or("firmware_job_missing")?;
        if self.outbox.len() == OUTBOX_LIMIT {
            return Err("firmware_outbox_capacity");
        }
        let status = FirmwareStatus201 {
            job: job.id,
            status: status.to_owned(),
            request_id: job.request_id,
        };
        self.outbox.push_back(status);
        Ok(())
    }

    fn finish(&mut self, status: &'static str) -> Result<(), &'static str> {
        self.queue(status)?;
        self.job_mut()?.phase = Phase::Finished;
        Ok(())
    }

    pub(super) fn accept(
        &mut self,
        request: &super::request::FirmwareRequest201,
        now: OffsetDateTime,
    ) -> Result<(), &'static str> {
        let id = self.jobs.checked_add(1).ok_or("firmware_job_capacity")?;
        self.jobs = id;
        let scheduled = request.retrieve_at > now;
        let secure = self.mode == FirmwareMode201::Secure;
        self.job = Some(Job {
            id,
            request_id: request.request_id,
            location: request.location.clone(),
            retrieve_at: format(request.retrieve_at)?,
            install_at: request.install_at.map(format).transpose()?,
            retries: request.retries,
            retry_interval: request.retry_interval,
            // A non-secure station has no manufacturer root and ignores signing material.
            signing_certificate: request.signing_certificate.clone().filter(|_| secure),
            signature: request.signature.clone().filter(|_| secure),
            phase: if scheduled {
                Phase::Scheduled
            } else {
                Phase::DownloadReady
            },
            attempts: 0,
            retry_at: None,
            sha256: None,
            signature_valid: None,
            sent: Vec::new(),
        });
        // L01.FR.13 / L02.FR.07: a download that does not start yet is reported scheduled.
        if scheduled {
            self.queue("DownloadScheduled")?;
        }
        Ok(())
    }

    /// L01.FR.24/27 for an ongoing job; `None` when nothing is ongoing.
    pub(super) fn cancel_active(&mut self, policy: CancelPolicy201) -> Option<&'static str> {
        if !self.active() {
            return None;
        }
        if policy == CancelPolicy201::Reject {
            return Some("Rejected");
        }
        let job = self.job.as_mut().expect("active job");
        job.phase = Phase::Finished;
        let id = job.request_id;
        if self.cancelled.len() == CANCELLED_LIMIT {
            self.cancelled.remove(0);
        }
        self.cancelled.push(id);
        // Statuses of the cancelled update that were never sent stay unsent.
        self.outbox.retain(|status| status.request_id != id);
        Some("AcceptedCanceled")
    }

    /// Advance every time- or fact-driven transition; returns the effects to perform.
    pub(super) fn advance(
        &mut self,
        now: OffsetDateTime,
        facts: StationFacts201,
        settings: &super::Settings,
        reboot: &mut RebootState,
    ) -> Result<FirmwareEffects201, &'static str> {
        let mut effects = FirmwareEffects201::default();
        let secure = self.mode == FirmwareMode201::Secure;
        for _ in 0..16 {
            let Some(job) = self.job.as_ref() else {
                return Ok(effects);
            };
            let next = match job.phase {
                Phase::Scheduled if due(&job.retrieve_at, now) => Phase::DownloadReady,
                Phase::RetryWait if job.retry_at.as_deref().is_none_or(|at| due(at, now)) => {
                    Phase::DownloadReady
                }
                Phase::DownloadReady => {
                    let attempt = job.attempts.checked_add(1).ok_or("firmware_attempts")?;
                    effects.download = Some(super::DownloadTicket201 {
                        job: job.id,
                        attempt,
                        simulated_failure: attempt <= settings.download_failures,
                        location: job.location.clone(),
                    });
                    self.queue("Downloading")?;
                    self.job_mut()?.attempts = attempt;
                    Phase::Downloading
                }
                // L01 scenario 5: a failed or missing signature ends the update.
                Phase::Downloaded if secure && job.signature_valid != Some(true) => {
                    self.finish("InvalidSignature")?;
                    continue;
                }
                Phase::Downloaded => {
                    let later = job.install_at.as_deref().is_some_and(|at| !due(at, now));
                    if secure {
                        self.queue("SignatureVerified")?;
                    }
                    // L01.FR.16 / L02.FR.10.
                    if later {
                        self.queue("InstallScheduled")?;
                        Phase::InstallScheduled
                    } else {
                        Phase::AwaitIdle
                    }
                }
                Phase::InstallScheduled
                    if job.install_at.as_deref().is_none_or(|at| due(at, now)) =>
                {
                    Phase::AwaitIdle
                }
                // L01.FR.06: installation waits for every transaction to end.
                Phase::AwaitIdle if facts.idle || !settings.wait_for_transactions => {
                    self.queue("Installing")?;
                    // L01.FR.29 and Figure 116: verification fails while installing.
                    if settings.fail_install_verification {
                        self.finish("InstallVerificationFailed")?;
                        continue;
                    }
                    if settings.fail_install {
                        self.finish("InstallationFailed")?;
                        continue;
                    }
                    if !settings.reboot_after_install {
                        self.install(settings, false)?;
                        continue;
                    }
                    // L01.FR.32 option (a): announce the reboot, then report Installed from
                    // the activated firmware.
                    self.queue("InstallRebooting")?;
                    *reboot = RebootState::Pending;
                    Phase::Rebooting
                }
                Phase::Rebooting => {
                    match (*reboot, facts.boot_generation) {
                        (RebootState::Idle | RebootState::Pending, Some(generation))
                            if self.outbox.is_empty() =>
                        {
                            *reboot = RebootState::Requested(generation);
                            effects.reboot = Some(self.version(settings));
                        }
                        (RebootState::Requested(prior), Some(current)) if current != prior => {
                            self.install(settings, true)?;
                            continue;
                        }
                        (RebootState::Restarted, Some(_)) => {
                            self.install(settings, true)?;
                            continue;
                        }
                        _ => {}
                    }
                    return Ok(effects);
                }
                _ => return Ok(effects),
            };
            self.job_mut()?.phase = next;
        }
        Ok(effects)
    }

    fn install(&mut self, settings: &super::Settings, rebooted: bool) -> Result<(), &'static str> {
        self.installed_version = Some(self.version(settings));
        if rebooted {
            self.reboots = self.reboots.saturating_add(1);
        }
        self.finish("Installed")
    }

    pub(super) fn version(&self, settings: &super::Settings) -> String {
        let digest = self
            .job
            .as_ref()
            .and_then(|job| job.sha256.as_deref())
            .unwrap_or("unknown");
        format!(
            "{}sha256-{}",
            settings.firmware_version_prefix,
            &digest[..digest.len().min(12)]
        )
    }

    /// L01.FR.30: `DownloadFailed` only after every retry attempt failed.
    pub(super) fn downloaded(
        &mut self,
        outcome: Option<(String, Option<bool>)>,
        now: OffsetDateTime,
    ) -> Result<(), &'static str> {
        let job = self.job_mut()?;
        match outcome {
            Some((sha256, signature_valid)) => {
                job.sha256 = Some(sha256);
                job.signature_valid = signature_valid;
                job.phase = Phase::Downloaded;
                self.queue("Downloaded")
            }
            None if job.attempts <= job.retries => {
                let delay = i64::from(job.retry_interval.min(RETRY_CAP_SECONDS));
                job.retry_at = Some(format(now + Duration::seconds(delay))?);
                job.phase = Phase::RetryWait;
                Ok(())
            }
            None => self.finish("DownloadFailed"),
        }
    }

    /// Status history appended once the CSMS answered the CALL.
    pub(super) fn record_sent(&mut self, status: &FirmwareStatus201) {
        self.last_sent = Some((status.status.clone(), status.request_id));
        if let Some(job) = self.job.as_mut().filter(|job| job.id == status.job) {
            if job.sent.len() == HISTORY_LIMIT {
                job.sent.remove(0);
            }
            job.sent.push(status.status.clone());
        }
    }
}

pub(super) fn parse(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn due(value: &str, now: OffsetDateTime) -> bool {
    parse(value).is_none_or(|at| at <= now)
}

fn format(value: OffsetDateTime) -> Result<String, &'static str> {
    value.format(&Rfc3339).map_err(|_| "firmware_timestamp")
}
