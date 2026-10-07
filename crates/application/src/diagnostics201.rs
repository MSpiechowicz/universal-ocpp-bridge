//! Application-owned OCPP 2.0.1 log upload job lifecycle (N01) and its atomic admission and
//! observation boundaries. Native notifications only advance a job attributed to them by their
//! exact `requestId` (N01.FR.07); they never create one. A station's `Uploaded` claim is
//! resolved against what the artifact provider actually stored for the job's destination.
use crate::StorageFuture;
use uob_contracts::{
    DiagnosticsJobState201, DiagnosticsUpload201, LogType201, LogUploadStatus201, RequestId,
    ResourceRef, UtcTimestamp,
};

/// Retained job revisions per station, including unresolved ones.
pub const MAX_DIAGNOSTICS_JOBS_201: usize = 64;

/// Captured before durable admission, in the same transaction as the command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticsJobMutation201 {
    pub station: ResourceRef,
    pub request_id: RequestId,
    /// Native `requestId`, unique among the station's retained jobs.
    pub native_request_id: i32,
    pub log_type: LogType201,
    pub admitted_at: UtcTimestamp,
    pub deadline: UtcTimestamp,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticsJobRecord201 {
    pub station: ResourceRef,
    pub request_id: RequestId,
    pub native_request_id: i32,
    pub log_type: LogType201,
    pub revision: u64,
    pub state: DiagnosticsJobState201,
    pub admitted_at: UtcTimestamp,
    pub changed_at: UtcTimestamp,
    pub deadline: UtcTimestamp,
    /// Whether dispatch started; an unsent job can be proven `not_sent` after restart.
    pub started: bool,
    /// Provider destination bound before the request could reach the station. Private: it
    /// never appears in results, history or exports.
    pub upload_id: Option<String>,
    pub last_status: Option<LogUploadStatus201>,
    pub last_status_at: Option<UtcTimestamp>,
    pub notifications: u32,
    /// Bridge-observed provider facts, present only for `uploaded`.
    pub upload: Option<DiagnosticsUpload201>,
}

/// What the provider held for one destination when the station reported `Uploaded`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UploadOutcome201 {
    Received(DiagnosticsUpload201),
    /// No complete upload, an unknown destination or an unavailable provider.
    Missing,
}

/// Provider check bound to the destination it was made for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UploadCheck201 {
    pub upload_id: String,
    pub outcome: UploadOutcome201,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticsObservationKind201 {
    /// One validated native notification. `request_id` is absent only for a triggered `Idle`
    /// (N01.FR.13). `upload` is present only for `Uploaded`.
    Status {
        status: LogUploadStatus201,
        request_id: Option<i32>,
        upload: Option<UploadCheck201>,
    },
    /// Trusted bridge clock check against job deadlines.
    Expiry,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticsObservation201 {
    pub station: ResourceRef,
    pub observed_at: UtcTimestamp,
    pub kind: DiagnosticsObservationKind201,
}

/// Same worker and transaction boundary as commands and other station observations.
pub trait DiagnosticsStore201: Send + Sync {
    fn diagnostics_jobs_201(
        &self,
        station: ResourceRef,
    ) -> StorageFuture<'_, Vec<DiagnosticsJobRecord201>>;
    /// Durably binds the provider destination to an admitted, unsent job. It must succeed
    /// before the native request is sent, so every reported upload can be checked.
    fn bind_diagnostics_upload_201(
        &self,
        request_id: RequestId,
        upload_id: String,
    ) -> StorageFuture<'_, ()>;
    /// Startup maintenance only. Never returns or queues a native request.
    fn recover_diagnostics_jobs_201(&self, now: UtcTimestamp) -> StorageFuture<'_, ()>;
    fn expire_diagnostics_jobs_201(&self, now: UtcTimestamp) -> StorageFuture<'_, ()>;
}

/// Effect of one attributed native status on a job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticsTransition201 {
    Advanced,
    /// The job is already resolved; the fact is counted but never revives it.
    Late,
}

/// Index of the job a native notification belongs to, if any.
///
/// `LogStatusNotification` matches only its exact `requestId` (N01.FR.07). An identity-free
/// `Idle` (N01.FR.13) settles only a job the station already answered; a `pending` or
/// `uncertain` job is left alone, because the station may not have seen the request.
#[must_use]
pub fn attribute_diagnostics_201(
    records: &[DiagnosticsJobRecord201],
    status: LogUploadStatus201,
    request_id: Option<i32>,
) -> Option<usize> {
    let mut newest_first = records.iter().enumerate().rev();
    match request_id {
        Some(wanted) => newest_first
            .find(|(_, record)| record.native_request_id == wanted)
            .map(|(index, _)| index),
        None if status == LogUploadStatus201::Idle => newest_first
            .find(|(_, record)| {
                matches!(
                    record.state,
                    DiagnosticsJobState201::Accepted
                        | DiagnosticsJobState201::Uploading
                        | DiagnosticsJobState201::TimedOut
                )
            })
            .map(|(index, _)| index),
        None => None,
    }
}

/// Applies one attributed native status. Any unresolved job may end in a native end state;
/// `Uploaded` becomes `uploaded` only when the provider holds a complete file for this job's
/// own destination, otherwise `upload_unconfirmed`. `AcceptedCanceled` reports that the
/// station cancelled this very upload (N01.FR.20).
pub fn apply_diagnostics_status_201(
    record: &mut DiagnosticsJobRecord201,
    status: LogUploadStatus201,
    upload: Option<&UploadCheck201>,
    at: UtcTimestamp,
) -> DiagnosticsTransition201 {
    record.notifications = record.notifications.saturating_add(1);
    if record.state.resolved() {
        return DiagnosticsTransition201::Late;
    }
    if status != LogUploadStatus201::Idle {
        record.last_status = Some(status);
        record.last_status_at = Some(at);
    }
    record.state = match status {
        LogUploadStatus201::Uploading => DiagnosticsJobState201::Uploading,
        LogUploadStatus201::Uploaded => match upload {
            Some(UploadCheck201 {
                upload_id,
                outcome: UploadOutcome201::Received(facts),
            }) if record.upload_id.as_deref() == Some(upload_id.as_str()) => {
                record.upload = Some(facts.clone());
                DiagnosticsJobState201::Uploaded
            }
            _ => DiagnosticsJobState201::UploadUnconfirmed,
        },
        LogUploadStatus201::UploadFailure => DiagnosticsJobState201::UploadFailed,
        LogUploadStatus201::BadMessage => DiagnosticsJobState201::BadMessage,
        LogUploadStatus201::NotSupportedOperation => DiagnosticsJobState201::NotSupportedOperation,
        LogUploadStatus201::PermissionDenied => DiagnosticsJobState201::PermissionDenied,
        LogUploadStatus201::AcceptedCanceled => DiagnosticsJobState201::Cancelled,
        LogUploadStatus201::Idle => DiagnosticsJobState201::StationIdle,
    };
    record.changed_at = record.changed_at.max(at);
    DiagnosticsTransition201::Advanced
}

#[cfg(test)]
mod tests;
