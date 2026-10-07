//! Application-owned OCPP 2.0.1 firmware job lifecycle (L01/L02) and its atomic admission and
//! observation boundaries. Native notifications only advance a job attributed to them by their
//! exact `requestId` (L01.FR.10); they never create one.
use crate::StorageFuture;
use uob_contracts::{FirmwareJobState201, FirmwareStatus201, RequestId, ResourceRef, UtcTimestamp};

/// Retained job revisions per station, including unresolved ones.
pub const MAX_FIRMWARE_JOBS_201: usize = 64;

/// Captured before durable admission, in the same transaction as the command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareJobMutation201 {
    pub station: ResourceRef,
    pub request_id: RequestId,
    /// Native `requestId`, unique among the station's retained jobs.
    pub native_request_id: i32,
    /// Secure (L01) request carrying a verified signing certificate and signature.
    pub secure: bool,
    pub artifact_reference: String,
    pub admitted_at: UtcTimestamp,
    pub deadline: UtcTimestamp,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareJobRecord201 {
    pub station: ResourceRef,
    pub request_id: RequestId,
    pub native_request_id: i32,
    pub secure: bool,
    pub artifact_reference: String,
    pub revision: u64,
    pub state: FirmwareJobState201,
    pub admitted_at: UtcTimestamp,
    pub changed_at: UtcTimestamp,
    pub deadline: UtcTimestamp,
    /// Whether dispatch started; an unsent job can be proven `not_sent` after restart.
    pub started: bool,
    pub last_status: Option<FirmwareStatus201>,
    pub last_status_at: Option<UtcTimestamp>,
    pub notifications: u32,
    pub rejected_transitions: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareObservationKind201 {
    /// One validated native notification; `request_id` is absent only for `Idle` (L01.FR.20).
    Status {
        status: FirmwareStatus201,
        request_id: Option<i32>,
    },
    /// Trusted bridge clock check against job deadlines.
    Expiry,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareObservation201 {
    pub station: ResourceRef,
    pub observed_at: UtcTimestamp,
    pub kind: FirmwareObservationKind201,
}

/// Same worker and transaction boundary as commands and other station observations.
pub trait FirmwareStore201: Send + Sync {
    fn firmware_jobs_201(
        &self,
        station: ResourceRef,
    ) -> StorageFuture<'_, Vec<FirmwareJobRecord201>>;
    /// Startup maintenance only. Never returns or queues a native request.
    fn recover_firmware_jobs_201(&self, now: UtcTimestamp) -> StorageFuture<'_, ()>;
    fn expire_firmware_jobs_201(&self, now: UtcTimestamp) -> StorageFuture<'_, ()>;
}

/// Effect of one attributed native status on a job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareTransition201 {
    Advanced,
    /// The job is already resolved; the fact is counted but never revives it.
    Late,
    /// A regression or contradiction; the state is unchanged and the fact counted.
    Rejected,
}

/// Progress order of Figure 116. Equal phases may alternate (retries, pause, reboot).
const fn phase(status: FirmwareStatus201) -> u8 {
    match status {
        FirmwareStatus201::Idle => 0,
        FirmwareStatus201::DownloadScheduled => 1,
        FirmwareStatus201::Downloading | FirmwareStatus201::DownloadPaused => 2,
        FirmwareStatus201::Downloaded | FirmwareStatus201::DownloadFailed => 3,
        FirmwareStatus201::SignatureVerified | FirmwareStatus201::InvalidSignature => 4,
        FirmwareStatus201::InstallScheduled => 5,
        FirmwareStatus201::InstallRebooting
        | FirmwareStatus201::Installing
        | FirmwareStatus201::InstallVerificationFailed => 6,
        FirmwareStatus201::Installed | FirmwareStatus201::InstallationFailed => 7,
    }
}

/// Job state reached by a native status.
#[must_use]
pub const fn firmware_state_201(status: FirmwareStatus201) -> FirmwareJobState201 {
    match status {
        FirmwareStatus201::Downloaded => FirmwareJobState201::Downloaded,
        FirmwareStatus201::DownloadFailed => FirmwareJobState201::DownloadFailed,
        FirmwareStatus201::Downloading => FirmwareJobState201::Downloading,
        FirmwareStatus201::DownloadScheduled => FirmwareJobState201::DownloadScheduled,
        FirmwareStatus201::DownloadPaused => FirmwareJobState201::DownloadPaused,
        FirmwareStatus201::Idle => FirmwareJobState201::StationIdle,
        FirmwareStatus201::InstallationFailed => FirmwareJobState201::InstallationFailed,
        FirmwareStatus201::Installing => FirmwareJobState201::Installing,
        FirmwareStatus201::Installed => FirmwareJobState201::Installed,
        FirmwareStatus201::InstallRebooting => FirmwareJobState201::InstallRebooting,
        FirmwareStatus201::InstallScheduled => FirmwareJobState201::InstallScheduled,
        FirmwareStatus201::InstallVerificationFailed => {
            FirmwareJobState201::InstallVerificationFailed
        }
        FirmwareStatus201::InvalidSignature => FirmwareJobState201::InvalidSignature,
        FirmwareStatus201::SignatureVerified => FirmwareJobState201::SignatureVerified,
    }
}

/// Applies one attributed native status. Progress never moves to an earlier phase; failure
/// end states must match the phase they report (a download cannot fail after it completed).
/// A non-secure update (L02) carries no signature, so signature statuses contradict it.
/// `Idle` reports that the station has no firmware work in progress and resolves the job.
pub fn apply_firmware_status_201(
    record: &mut FirmwareJobRecord201,
    status: FirmwareStatus201,
    at: UtcTimestamp,
) -> FirmwareTransition201 {
    record.notifications = record.notifications.saturating_add(1);
    if record.state.resolved() {
        return FirmwareTransition201::Late;
    }
    let floor = record.last_status.map_or(0, phase);
    let signature = matches!(
        status,
        FirmwareStatus201::SignatureVerified | FirmwareStatus201::InvalidSignature
    );
    let allowed = (record.secure || !signature)
        && match status {
            FirmwareStatus201::Idle
            | FirmwareStatus201::Installed
            | FirmwareStatus201::InstallationFailed => true,
            FirmwareStatus201::DownloadFailed => floor <= 2,
            FirmwareStatus201::InvalidSignature => floor <= 3,
            FirmwareStatus201::InstallVerificationFailed => floor <= 6,
            progress => phase(progress) >= floor,
        };
    if !allowed {
        record.rejected_transitions = record.rejected_transitions.saturating_add(1);
        return FirmwareTransition201::Rejected;
    }
    if status != FirmwareStatus201::Idle {
        record.last_status = Some(status);
        record.last_status_at = Some(at);
    }
    record.state = firmware_state_201(status);
    record.changed_at = record.changed_at.max(at);
    FirmwareTransition201::Advanced
}

#[cfg(test)]
mod tests;
