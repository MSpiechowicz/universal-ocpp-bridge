use serde::{Deserialize, Serialize};
use uob_application::{
    DiagnosticsJobRecord16, DiagnosticsVariant16, StorageError, StorageErrorCode,
    artifact_provider::UploadId,
};
use uob_contracts::{
    DiagnosticsJobState16, DiagnosticsUpload16, LogType16, LogUploadStatus16, RequestId,
    ResourceRef, UtcTimestamp,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    station: ResourceRef,
    request_id: RequestId,
    /// Present exactly for log jobs, together with `log_type`.
    native_request_id: Option<i32>,
    log_type: Option<LogType16>,
    revision: u64,
    state: DiagnosticsJobState16,
    admitted_at: UtcTimestamp,
    changed_at: UtcTimestamp,
    deadline: UtcTimestamp,
    started: bool,
    upload_id: Option<String>,
    last_status: Option<LogUploadStatus16>,
    last_status_at: Option<UtcTimestamp>,
    notifications: u32,
    rejected_transitions: u32,
    upload: Option<DiagnosticsUpload16>,
}

fn error() -> StorageError {
    StorageError::new(
        StorageErrorCode::IntegrityFailure,
        "invalid diagnostics job record",
    )
}

/// Same syntax the provider port enforces for its upload identities.
pub(crate) fn validate_upload_id(value: Option<&str>) -> Result<(), StorageError> {
    value.map_or(Ok(()), |value| {
        UploadId::new(value).map(|_| ()).map_err(|_| error())
    })
}

pub(crate) fn encode(record: &DiagnosticsJobRecord16) -> Result<String, StorageError> {
    let (native_request_id, log_type) = match record.variant {
        DiagnosticsVariant16::Diagnostics => (None, None),
        DiagnosticsVariant16::Log {
            log_type,
            request_id,
        } => (Some(request_id), Some(log_type)),
    };
    serde_json::to_string(&Record {
        station: record.station.clone(),
        request_id: record.request_id.clone(),
        native_request_id,
        log_type,
        revision: record.revision,
        state: record.state,
        admitted_at: record.admitted_at,
        changed_at: record.changed_at,
        deadline: record.deadline,
        started: record.started,
        upload_id: record.upload_id.clone(),
        last_status: record.last_status,
        last_status_at: record.last_status_at,
        notifications: record.notifications,
        rejected_transitions: record.rejected_transitions,
        upload: record.upload.clone(),
    })
    .map_err(|_| error())
}

pub(crate) fn decode(value: &str) -> Result<DiagnosticsJobRecord16, StorageError> {
    let record: Record = serde_json::from_str(value).map_err(|_| error())?;
    let variant = match (record.native_request_id, record.log_type) {
        (None, None) => DiagnosticsVariant16::Diagnostics,
        (Some(request_id), Some(log_type)) => DiagnosticsVariant16::Log {
            log_type,
            request_id,
        },
        _ => return Err(error()),
    };
    validate_upload_id(record.upload_id.as_deref())?;
    let family = |status: LogUploadStatus16| match variant {
        DiagnosticsVariant16::Diagnostics => status.diagnostics(),
        DiagnosticsVariant16::Log { .. } => status.log(),
    };
    if record.revision == 0
        || record.station.resource.is_some()
        || record.changed_at < record.admitted_at
        || record.last_status.is_some() != record.last_status_at.is_some()
        || record.last_status == Some(LogUploadStatus16::Idle)
        || record.last_status.is_some_and(|status| !family(status))
        || (record.upload_id.is_some() && !record.started)
        || record.upload.is_some() != (record.state == DiagnosticsJobState16::Uploaded)
        || record.upload.as_ref().is_some_and(|upload| {
            upload.sha256.len() != 64
                || !upload
                    .sha256
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
    {
        return Err(error());
    }
    Ok(DiagnosticsJobRecord16 {
        station: record.station,
        request_id: record.request_id,
        variant,
        revision: record.revision,
        state: record.state,
        admitted_at: record.admitted_at,
        changed_at: record.changed_at,
        deadline: record.deadline,
        started: record.started,
        upload_id: record.upload_id,
        last_status: record.last_status,
        last_status_at: record.last_status_at,
        notifications: record.notifications,
        rejected_transitions: record.rejected_transitions,
        upload: record.upload,
    })
}
