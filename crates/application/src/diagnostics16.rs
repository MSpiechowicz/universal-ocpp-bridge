//! Application-owned OCPP 1.6 diagnostics and log upload job lifecycle and its atomic
//! admission/observation boundaries. Native notifications only advance a job attributed to
//! them; they never create one. A station's `Uploaded` claim is resolved against what the
//! artifact provider actually stored for the job's destination.
use crate::StorageFuture;
use uob_contracts::{
    DiagnosticsJobState16, DiagnosticsUpload16, LogType16, LogUploadStatus16, RequestId,
    ResourceRef, UtcTimestamp,
};

/// Retained job revisions per station, including unresolved ones.
pub const MAX_DIAGNOSTICS_JOBS_16: usize = 64;

/// Which native message family owns a job. Log jobs are matched only by `requestId`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticsVariant16 {
    /// OCPP 1.6 `GetDiagnostics` / `DiagnosticsStatusNotification`.
    Diagnostics,
    /// Security Whitepaper `GetLog` / `LogStatusNotification`.
    Log {
        log_type: LogType16,
        request_id: i32,
    },
}

impl DiagnosticsVariant16 {
    #[must_use]
    pub const fn log_type(self) -> LogType16 {
        match self {
            Self::Diagnostics => LogType16::DiagnosticsLog,
            Self::Log { log_type, .. } => log_type,
        }
    }
}

/// Captured before durable admission, in the same transaction as the command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticsJobMutation16 {
    pub station: ResourceRef,
    pub request_id: RequestId,
    pub variant: DiagnosticsVariant16,
    pub admitted_at: UtcTimestamp,
    pub deadline: UtcTimestamp,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticsJobRecord16 {
    pub station: ResourceRef,
    pub request_id: RequestId,
    pub variant: DiagnosticsVariant16,
    pub revision: u64,
    pub state: DiagnosticsJobState16,
    pub admitted_at: UtcTimestamp,
    pub changed_at: UtcTimestamp,
    pub deadline: UtcTimestamp,
    /// Whether dispatch started; an unsent job can be proven `not_sent` after restart.
    pub started: bool,
    /// Provider destination bound before the request could reach the station. Private: it
    /// never appears in results, history or exports.
    pub upload_id: Option<String>,
    pub last_status: Option<LogUploadStatus16>,
    pub last_status_at: Option<UtcTimestamp>,
    pub notifications: u32,
    pub rejected_transitions: u32,
    /// Bridge-observed provider facts, present only for `uploaded`.
    pub upload: Option<DiagnosticsUpload16>,
}

/// What the provider held for one destination when the station reported `Uploaded`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UploadOutcome16 {
    Received(DiagnosticsUpload16),
    /// No complete upload, an unknown destination or an unavailable provider.
    Missing,
}

/// Provider check bound to the destination it was made for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UploadCheck16 {
    pub upload_id: String,
    pub outcome: UploadOutcome16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticsObservationKind16 {
    /// One validated native notification. `log` selects `LogStatusNotification`, whose
    /// `request_id` is absent only for a triggered `Idle` (N01.FR.12). `upload` is present
    /// only for `Uploaded`.
    Status {
        status: LogUploadStatus16,
        log: bool,
        request_id: Option<i32>,
        upload: Option<UploadCheck16>,
    },
    /// Trusted bridge clock check against job deadlines.
    Expiry,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticsObservation16 {
    pub station: ResourceRef,
    pub observed_at: UtcTimestamp,
    pub kind: DiagnosticsObservationKind16,
}

/// Same worker and transaction boundary as commands and other station observations.
pub trait DiagnosticsStore16: Send + Sync {
    fn diagnostics_jobs_16(
        &self,
        station: ResourceRef,
    ) -> StorageFuture<'_, Vec<DiagnosticsJobRecord16>>;
    /// Durably binds the provider destination to an admitted, unsent job. It must succeed
    /// before the native request is sent, so every reported upload can be checked.
    fn bind_diagnostics_upload_16(
        &self,
        request_id: RequestId,
        upload_id: String,
    ) -> StorageFuture<'_, ()>;
    /// Startup maintenance only. Never returns or queues a native request.
    fn recover_diagnostics_jobs_16(&self, now: UtcTimestamp) -> StorageFuture<'_, ()>;
    fn expire_diagnostics_jobs_16(&self, now: UtcTimestamp) -> StorageFuture<'_, ()>;
}

/// Effect of one attributed native status on a job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticsTransition16 {
    Advanced,
    /// The job is already resolved; the fact is counted but never revives it.
    Late,
    /// A status outside the job's message family; the state is unchanged and the fact counted.
    Rejected,
}

/// Index of the job a native notification belongs to, if any.
///
/// `DiagnosticsStatusNotification` carries no identity, so it belongs to the newest unresolved
/// diagnostics job. `LogStatusNotification` matches only its exact `requestId` (N01.FR.07); an
/// identity-free `Idle` (N01.FR.12) settles only a log job the station already answered.
#[must_use]
pub fn attribute_diagnostics_16(
    records: &[DiagnosticsJobRecord16],
    status: LogUploadStatus16,
    log: bool,
    request_id: Option<i32>,
) -> Option<usize> {
    let mut newest_first = records.iter().enumerate().rev();
    match (log, request_id) {
        (false, _) => newest_first
            .find(|(_, record)| {
                record.variant == DiagnosticsVariant16::Diagnostics && !record.state.resolved()
            })
            .map(|(index, _)| index),
        (true, Some(wanted)) => newest_first
            .find(|(_, record)| {
                matches!(record.variant, DiagnosticsVariant16::Log { request_id, .. } if request_id == wanted)
            })
            .map(|(index, _)| index),
        (true, None) if status == LogUploadStatus16::Idle => newest_first
            .find(|(_, record)| {
                matches!(record.variant, DiagnosticsVariant16::Log { .. })
                    && matches!(
                        record.state,
                        DiagnosticsJobState16::Accepted
                            | DiagnosticsJobState16::Uploading
                            | DiagnosticsJobState16::TimedOut
                    )
            })
            .map(|(index, _)| index),
        (true, None) => None,
    }
}

/// Applies one attributed native status. Any unresolved job may end in a native end state;
/// `Uploaded` becomes `uploaded` only when the provider holds a complete file for this job's
/// own destination, otherwise `upload_unconfirmed`.
pub fn apply_diagnostics_status_16(
    record: &mut DiagnosticsJobRecord16,
    status: LogUploadStatus16,
    upload: Option<&UploadCheck16>,
    at: UtcTimestamp,
) -> DiagnosticsTransition16 {
    record.notifications = record.notifications.saturating_add(1);
    if record.state.resolved() {
        return DiagnosticsTransition16::Late;
    }
    let family = match record.variant {
        DiagnosticsVariant16::Diagnostics => status.diagnostics(),
        DiagnosticsVariant16::Log { .. } => status.log(),
    };
    if !family {
        record.rejected_transitions = record.rejected_transitions.saturating_add(1);
        return DiagnosticsTransition16::Rejected;
    }
    if status != LogUploadStatus16::Idle {
        record.last_status = Some(status);
        record.last_status_at = Some(at);
    }
    record.state = match status {
        LogUploadStatus16::Uploading => DiagnosticsJobState16::Uploading,
        LogUploadStatus16::Uploaded => match upload {
            Some(UploadCheck16 {
                upload_id,
                outcome: UploadOutcome16::Received(facts),
            }) if record.upload_id.as_deref() == Some(upload_id.as_str()) => {
                record.upload = Some(facts.clone());
                DiagnosticsJobState16::Uploaded
            }
            _ => DiagnosticsJobState16::UploadUnconfirmed,
        },
        LogUploadStatus16::UploadFailed | LogUploadStatus16::UploadFailure => {
            DiagnosticsJobState16::UploadFailed
        }
        LogUploadStatus16::BadMessage => DiagnosticsJobState16::BadMessage,
        LogUploadStatus16::NotSupportedOperation => DiagnosticsJobState16::NotSupportedOperation,
        LogUploadStatus16::PermissionDenied => DiagnosticsJobState16::PermissionDenied,
        LogUploadStatus16::Idle => DiagnosticsJobState16::StationIdle,
    };
    record.changed_at = record.changed_at.max(at);
    DiagnosticsTransition16::Advanced
}

#[cfg(test)]
mod tests;
