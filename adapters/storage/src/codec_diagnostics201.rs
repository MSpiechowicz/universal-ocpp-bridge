use serde::{Deserialize, Serialize};
use uob_application::{
    DiagnosticsJobRecord201, StorageError, StorageErrorCode, artifact_provider::UploadId,
};
use uob_contracts::{
    DiagnosticsJobState201, DiagnosticsUpload201, LogType201, LogUploadStatus201, RequestId,
    ResourceRef, UtcTimestamp,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    station: ResourceRef,
    request_id: RequestId,
    native_request_id: i32,
    log_type: LogType201,
    revision: u64,
    state: DiagnosticsJobState201,
    admitted_at: UtcTimestamp,
    changed_at: UtcTimestamp,
    deadline: UtcTimestamp,
    started: bool,
    upload_id: Option<String>,
    last_status: Option<LogUploadStatus201>,
    last_status_at: Option<UtcTimestamp>,
    notifications: u32,
    upload: Option<DiagnosticsUpload201>,
}

fn error() -> StorageError {
    StorageError::new(
        StorageErrorCode::IntegrityFailure,
        "invalid log upload job record",
    )
}

/// Same syntax the provider port enforces for its upload identities.
pub(crate) fn validate_upload_id(value: Option<&str>) -> Result<(), StorageError> {
    value.map_or(Ok(()), |value| {
        UploadId::new(value).map(|_| ()).map_err(|_| error())
    })
}

pub(crate) fn valid_upload(upload: &DiagnosticsUpload201) -> bool {
    upload.sha256.len() == 64
        && upload
            .sha256
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

pub(crate) fn encode(record: &DiagnosticsJobRecord201) -> Result<String, StorageError> {
    serde_json::to_string(&Record {
        station: record.station.clone(),
        request_id: record.request_id.clone(),
        native_request_id: record.native_request_id,
        log_type: record.log_type,
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
        upload: record.upload.clone(),
    })
    .map_err(|_| error())
}

pub(crate) fn decode(value: &str) -> Result<DiagnosticsJobRecord201, StorageError> {
    let record: Record = serde_json::from_str(value).map_err(|_| error())?;
    validate_upload_id(record.upload_id.as_deref())?;
    if record.revision == 0
        || record.station.resource.is_some()
        || record.changed_at < record.admitted_at
        || record.last_status.is_some() != record.last_status_at.is_some()
        || record.last_status == Some(LogUploadStatus201::Idle)
        || (record.upload_id.is_some() && !record.started)
        || record.upload.is_some() != (record.state == DiagnosticsJobState201::Uploaded)
        || record
            .upload
            .as_ref()
            .is_some_and(|upload| !valid_upload(upload))
    {
        return Err(error());
    }
    Ok(DiagnosticsJobRecord201 {
        station: record.station,
        request_id: record.request_id,
        native_request_id: record.native_request_id,
        log_type: record.log_type,
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
        upload: record.upload,
    })
}
