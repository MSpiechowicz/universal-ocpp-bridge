//! Durable OCPP 2.0.1 station log-upload state and its native transitions (N01).
use super::config::{LogCancelPolicy201, Settings};
use super::request::{LogKind201, LogRequest201};
use super::{LogStatus201, UploadTicket201};
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, fmt::Write as _};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use zeroize::Zeroize;

const HISTORY_LIMIT: usize = 32;
const OUTBOX_LIMIT: usize = 64;
const CANCELLED_LIMIT: usize = 16;
const RETRY_CAP_SECONDS: u32 = 60;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    UploadReady,
    Uploading,
    RetryWait,
    Finished,
}

#[derive(Clone, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Job {
    pub(super) id: u64,
    pub(super) kind: LogKind201,
    pub(super) request_id: i32,
    pub(super) location: String,
    pub(super) file_name: String,
    pub(super) retries: u32,
    pub(super) retry_interval: u32,
    pub(super) oldest: Option<String>,
    pub(super) latest: Option<String>,
    pub(super) phase: Phase,
    pub(super) attempts: u32,
    pub(super) retry_at: Option<String>,
    pub(super) uploaded: bool,
    pub(super) sent: Vec<String>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.location.zeroize();
    }
}

#[derive(Clone, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct PrivateState {
    pub(super) format_version: u16,
    pub(super) station: String,
    pub(super) jobs: u64,
    pub(super) job: Option<Job>,
    pub(super) outbox: VecDeque<LogStatus201>,
    /// `requestId`s of uploads this station cancelled for a newer request (N01.FR.12).
    pub(super) cancelled: Vec<i32>,
    pub(super) uploads: u64,
    pub(super) revision: u64,
}

impl PrivateState {
    pub(super) fn empty(station: &str) -> Self {
        Self {
            format_version: 1,
            station: station.to_owned(),
            jobs: 0,
            job: None,
            outbox: VecDeque::new(),
            cancelled: Vec::new(),
            uploads: 0,
            revision: 0,
        }
    }

    pub(super) fn validate(&self, station: &str) -> Result<(), &'static str> {
        let job_valid = self.job.as_ref().is_none_or(|job| {
            job.id != 0
                && job.id <= self.jobs
                && !job.file_name.is_empty()
                && job.file_name.len() <= 255
                && job.oldest.as_deref().is_none_or(|at| parse(at).is_some())
                && job.latest.as_deref().is_none_or(|at| parse(at).is_some())
                && job.retry_at.as_deref().is_none_or(|at| parse(at).is_some())
                && job.sent.len() <= HISTORY_LIMIT
        });
        if self.format_version != 1
            || self.station != station
            || !job_valid
            || self.outbox.len() > OUTBOX_LIMIT
            || self.cancelled.len() > CANCELLED_LIMIT
            || self.uploads > self.jobs
        {
            return Err("diagnostics_state_invalid");
        }
        Ok(())
    }

    /// A process restart loses any in-flight upload, so that attempt starts again.
    pub(super) fn resume(&mut self) {
        if let Some(job) = self
            .job
            .as_mut()
            .filter(|job| job.phase == Phase::Uploading)
        {
            job.phase = Phase::UploadReady;
            job.attempts = job.attempts.saturating_sub(1);
        }
    }

    pub(super) fn active(&self) -> bool {
        self.job
            .as_ref()
            .is_some_and(|job| job.phase != Phase::Finished)
    }

    fn job_mut(&mut self) -> Result<&mut Job, &'static str> {
        self.job.as_mut().ok_or("diagnostics_job_missing")
    }

    /// N01.FR.07: every status is reported with the request's own requestId.
    fn queue(&mut self, status: &'static str) -> Result<(), &'static str> {
        let job = self.job.as_ref().ok_or("diagnostics_job_missing")?;
        if self.outbox.len() == OUTBOX_LIMIT {
            return Err("diagnostics_outbox_capacity");
        }
        let status = LogStatus201 {
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

    /// Starts one upload job and returns the file name reported in the reply.
    pub(super) fn accept(&mut self, request: &LogRequest201) -> Result<String, &'static str> {
        let id = self.jobs.checked_add(1).ok_or("diagnostics_job_capacity")?;
        self.jobs = id;
        let file_name = file_name(request.kind, &self.station, id);
        self.job = Some(Job {
            id,
            kind: request.kind,
            request_id: request.request_id,
            location: request.location.clone(),
            file_name: file_name.clone(),
            retries: request.retries,
            retry_interval: request.retry_interval,
            oldest: request.oldest.map(format).transpose()?,
            latest: request.latest.map(format).transpose()?,
            phase: Phase::UploadReady,
            attempts: 0,
            retry_at: None,
            uploaded: false,
            sent: Vec::new(),
        });
        Ok(file_name)
    }

    /// N01.FR.12 for an ongoing job; `None` when nothing is ongoing. A cancelled upload sends no
    /// further progress, so its undelivered statuses leave the outbox; it reports exactly one
    /// `AcceptedCanceled` with its own requestId (N01.FR.20).
    pub(super) fn cancel_active(
        &mut self,
        policy: LogCancelPolicy201,
    ) -> Result<Option<&'static str>, &'static str> {
        if !self.active() {
            return Ok(None);
        }
        if policy == LogCancelPolicy201::Reject {
            return Ok(Some("Rejected"));
        }
        let job = self.job.as_ref().expect("active job");
        let (id, request_id) = (job.id, job.request_id);
        self.outbox.retain(|status| status.job != id);
        self.finish("AcceptedCanceled")?;
        if self.cancelled.len() == CANCELLED_LIMIT {
            self.cancelled.remove(0);
        }
        self.cancelled.push(request_id);
        Ok(Some("AcceptedCanceled"))
    }

    /// Advance every time-driven transition; returns the upload attempt to perform.
    pub(super) fn advance(
        &mut self,
        now: OffsetDateTime,
        settings: &Settings,
    ) -> Result<Option<UploadTicket201>, &'static str> {
        let Some(job) = self.job.as_ref() else {
            return Ok(None);
        };
        match job.phase {
            Phase::RetryWait if job.retry_at.as_deref().is_none_or(|at| due(at, now)) => {
                self.job_mut()?.phase = Phase::UploadReady;
                self.advance(now, settings)
            }
            Phase::UploadReady => {
                let attempt = job.attempts.checked_add(1).ok_or("diagnostics_attempts")?;
                let ticket = UploadTicket201 {
                    job: job.id,
                    attempt,
                    simulated_failure: attempt <= settings.upload_failures,
                    location: job.location.clone(),
                    file_name: job.file_name.clone(),
                    content: content(job, &self.station, settings),
                };
                // N01.FR.08; a station MAY report Uploading again for each retry (N01.FR.10).
                self.queue("Uploading")?;
                let job = self.job_mut()?;
                job.attempts = attempt;
                job.phase = Phase::Uploading;
                Ok(Some(ticket))
            }
            _ => Ok(None),
        }
    }

    pub(super) fn uploaded(
        &mut self,
        success: bool,
        now: OffsetDateTime,
        settings: &Settings,
    ) -> Result<(), &'static str> {
        let job = self.job_mut()?;
        if success {
            job.uploaded = true;
            self.uploads = self.uploads.saturating_add(1);
            // N01.FR.09.
            return self.finish("Uploaded");
        }
        if job.attempts <= job.retries {
            let delay = i64::from(job.retry_interval.min(RETRY_CAP_SECONDS));
            job.retry_at = Some(format(now + Duration::seconds(delay))?);
            job.phase = Phase::RetryWait;
            return Ok(());
        }
        // N01.FR.10: a failure status only after every retry attempt failed.
        self.finish(settings.failure_status.as_str())
    }

    /// Status history appended once the CSMS answered the CALL.
    pub(super) fn record_sent(&mut self, status: &LogStatus201) {
        if let Some(job) = self.job.as_mut().filter(|job| job.id == status.job) {
            if job.sent.len() == HISTORY_LIMIT {
                job.sent.remove(0);
            }
            job.sent.push(status.status.clone());
        }
    }
}

/// `CiString255` file name; only characters a directory location can safely carry.
fn file_name(kind: LogKind201, station: &str, id: u64) -> String {
    let prefix = match kind {
        LogKind201::DiagnosticsLog => "diagnosticslog",
        LogKind201::SecurityLog => "securitylog",
    };
    let station: String = station
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(64)
        .collect();
    let station = if station.is_empty() {
        "station".to_owned()
    } else {
        station
    };
    format!("{prefix}-{station}-{id}.log")
}

/// Deterministic generated log content of the configured size; never the upload location.
fn content(job: &Job, station: &str, settings: &Settings) -> Vec<u8> {
    let (header, size) = match job.kind {
        LogKind201::SecurityLog => ("UOB-SIM SECURITY LOG", settings.security_log_bytes),
        LogKind201::DiagnosticsLog => ("UOB-SIM DIAGNOSTICS LOG", settings.diagnostics_bytes),
    };
    let mut text = format!(
        "{header}\nstation={station}\njob={}\nfile={}\nfrom={}\nto={}\n",
        job.id,
        job.file_name,
        job.oldest.as_deref().unwrap_or("-"),
        job.latest.as_deref().unwrap_or("-"),
    );
    let mut line = 0_u64;
    while u64::try_from(text.len()).is_ok_and(|length| length < size) {
        line += 1;
        let _ = writeln!(text, "{line:08} simulated entry");
    }
    let mut bytes = text.into_bytes();
    bytes.truncate(usize::try_from(size).unwrap_or(usize::MAX));
    bytes
}

pub(super) fn parse(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn due(value: &str, now: OffsetDateTime) -> bool {
    parse(value).is_none_or(|at| at <= now)
}

fn format(value: OffsetDateTime) -> Result<String, &'static str> {
    value.format(&Rfc3339).map_err(|_| "diagnostics_timestamp")
}
