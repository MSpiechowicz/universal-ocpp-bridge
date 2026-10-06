//! Application-owned OCPP 1.6 firmware job lifecycle and its atomic admission/observation
//! boundaries. Native notifications only advance a job attributed to them; they never create one.
use crate::StorageFuture;
use uob_contracts::{FirmwareJobState16, FirmwareStatus16, RequestId, ResourceRef, UtcTimestamp};

/// Retained job revisions per station, including unresolved ones.
pub const MAX_FIRMWARE_JOBS_16: usize = 64;

/// Which native message family owns a job. Signed jobs are matched only by `requestId`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareVariant16 {
    Legacy,
    Signed { request_id: i32 },
}

/// Captured before durable admission, in the same transaction as the command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareJobMutation16 {
    pub station: ResourceRef,
    pub request_id: RequestId,
    pub variant: FirmwareVariant16,
    pub artifact_reference: String,
    pub admitted_at: UtcTimestamp,
    pub deadline: UtcTimestamp,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareJobRecord16 {
    pub station: ResourceRef,
    pub request_id: RequestId,
    pub variant: FirmwareVariant16,
    pub artifact_reference: String,
    pub revision: u64,
    pub state: FirmwareJobState16,
    pub admitted_at: UtcTimestamp,
    pub changed_at: UtcTimestamp,
    pub deadline: UtcTimestamp,
    /// Whether dispatch started; an unsent job can be proven `not_sent` after restart.
    pub started: bool,
    pub last_status: Option<FirmwareStatus16>,
    pub last_status_at: Option<UtcTimestamp>,
    pub notifications: u32,
    pub rejected_transitions: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareObservationKind16 {
    /// One validated native notification. `request_id` is present only for signed messages.
    Status {
        status: FirmwareStatus16,
        signed: bool,
        request_id: Option<i32>,
    },
    /// Trusted bridge clock check against job deadlines.
    Expiry,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareObservation16 {
    pub station: ResourceRef,
    pub observed_at: UtcTimestamp,
    pub kind: FirmwareObservationKind16,
}

/// Same worker and transaction boundary as commands and other station observations.
pub trait FirmwareStore16: Send + Sync {
    fn firmware_jobs_16(&self, station: ResourceRef)
    -> StorageFuture<'_, Vec<FirmwareJobRecord16>>;
    /// Startup maintenance only. Never returns or queues a native request.
    fn recover_firmware_jobs_16(&self, now: UtcTimestamp) -> StorageFuture<'_, ()>;
    fn expire_firmware_jobs_16(&self, now: UtcTimestamp) -> StorageFuture<'_, ()>;
}

/// Effect of one attributed native status on a job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareTransition16 {
    Advanced,
    /// The job is already resolved; the fact is counted but never revives it.
    Late,
    /// A regression or contradiction; the state is unchanged and the fact counted.
    Rejected,
}

/// Progress order within one update. Equal phases may alternate (retries, pause, reboot).
const fn phase(status: FirmwareStatus16) -> u8 {
    match status {
        FirmwareStatus16::Idle => 0,
        FirmwareStatus16::DownloadScheduled => 1,
        FirmwareStatus16::Downloading | FirmwareStatus16::DownloadPaused => 2,
        FirmwareStatus16::Downloaded | FirmwareStatus16::DownloadFailed => 3,
        FirmwareStatus16::SignatureVerified | FirmwareStatus16::InvalidSignature => 4,
        FirmwareStatus16::InstallScheduled => 5,
        FirmwareStatus16::InstallRebooting
        | FirmwareStatus16::Installing
        | FirmwareStatus16::InstallVerificationFailed => 6,
        FirmwareStatus16::Installed | FirmwareStatus16::InstallationFailed => 7,
    }
}

/// Job state reached by a native status.
#[must_use]
pub const fn firmware_state_16(status: FirmwareStatus16) -> FirmwareJobState16 {
    match status {
        FirmwareStatus16::Downloaded => FirmwareJobState16::Downloaded,
        FirmwareStatus16::DownloadFailed => FirmwareJobState16::DownloadFailed,
        FirmwareStatus16::Downloading => FirmwareJobState16::Downloading,
        FirmwareStatus16::DownloadScheduled => FirmwareJobState16::DownloadScheduled,
        FirmwareStatus16::DownloadPaused => FirmwareJobState16::DownloadPaused,
        FirmwareStatus16::Idle => FirmwareJobState16::StationIdle,
        FirmwareStatus16::InstallationFailed => FirmwareJobState16::InstallationFailed,
        FirmwareStatus16::Installing => FirmwareJobState16::Installing,
        FirmwareStatus16::Installed => FirmwareJobState16::Installed,
        FirmwareStatus16::InstallRebooting => FirmwareJobState16::InstallRebooting,
        FirmwareStatus16::InstallScheduled => FirmwareJobState16::InstallScheduled,
        FirmwareStatus16::InstallVerificationFailed => {
            FirmwareJobState16::InstallVerificationFailed
        }
        FirmwareStatus16::InvalidSignature => FirmwareJobState16::InvalidSignature,
        FirmwareStatus16::SignatureVerified => FirmwareJobState16::SignatureVerified,
    }
}

/// Applies one attributed native status. Progress never moves to an earlier phase; failure
/// end states must match the phase they report (a download cannot fail after it completed).
/// `Idle` means the station reports no firmware work (1.6 §4.5, L01.FR.28) and resolves the job.
pub fn apply_firmware_status_16(
    record: &mut FirmwareJobRecord16,
    status: FirmwareStatus16,
    at: UtcTimestamp,
) -> FirmwareTransition16 {
    record.notifications = record.notifications.saturating_add(1);
    if record.state.resolved() {
        return FirmwareTransition16::Late;
    }
    let signed = matches!(record.variant, FirmwareVariant16::Signed { .. });
    let floor = record.last_status.map_or(0, phase);
    let allowed = (signed || status.legacy())
        && match status {
            FirmwareStatus16::Idle
            | FirmwareStatus16::Installed
            | FirmwareStatus16::InstallationFailed => true,
            // A failure must not contradict progress already reported for this update.
            FirmwareStatus16::DownloadFailed => floor <= 2,
            FirmwareStatus16::InvalidSignature => floor <= 3,
            FirmwareStatus16::InstallVerificationFailed => floor <= 6,
            progress => phase(progress) >= floor,
        };
    if !allowed {
        record.rejected_transitions = record.rejected_transitions.saturating_add(1);
        return FirmwareTransition16::Rejected;
    }
    if status != FirmwareStatus16::Idle {
        record.last_status = Some(status);
        record.last_status_at = Some(at);
    }
    record.state = firmware_state_16(status);
    record.changed_at = record.changed_at.max(at);
    FirmwareTransition16::Advanced
}

#[cfg(test)]
mod tests;
