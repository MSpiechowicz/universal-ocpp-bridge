use serde::{Deserialize, Serialize};
use uob_application::{FirmwareJobRecord201, StorageError, StorageErrorCode};
use uob_contracts::{FirmwareJobState201, FirmwareStatus201, RequestId, ResourceRef, UtcTimestamp};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    station: ResourceRef,
    request_id: RequestId,
    native_request_id: i32,
    secure: bool,
    artifact_reference: String,
    revision: u64,
    state: FirmwareJobState201,
    admitted_at: UtcTimestamp,
    changed_at: UtcTimestamp,
    deadline: UtcTimestamp,
    started: bool,
    last_status: Option<FirmwareStatus201>,
    last_status_at: Option<UtcTimestamp>,
    notifications: u32,
    rejected_transitions: u32,
}

fn error() -> StorageError {
    StorageError::new(
        StorageErrorCode::IntegrityFailure,
        "invalid firmware job record",
    )
}

pub(crate) fn encode(record: &FirmwareJobRecord201) -> Result<String, StorageError> {
    serde_json::to_string(&Record {
        station: record.station.clone(),
        request_id: record.request_id.clone(),
        native_request_id: record.native_request_id,
        secure: record.secure,
        artifact_reference: record.artifact_reference.clone(),
        revision: record.revision,
        state: record.state,
        admitted_at: record.admitted_at,
        changed_at: record.changed_at,
        deadline: record.deadline,
        started: record.started,
        last_status: record.last_status,
        last_status_at: record.last_status_at,
        notifications: record.notifications,
        rejected_transitions: record.rejected_transitions,
    })
    .map_err(|_| error())
}

pub(crate) fn decode(value: &str) -> Result<FirmwareJobRecord201, StorageError> {
    let record: Record = serde_json::from_str(value).map_err(|_| error())?;
    let signature = matches!(
        record.last_status,
        Some(FirmwareStatus201::SignatureVerified | FirmwareStatus201::InvalidSignature)
    );
    if record.revision == 0
        || record.station.resource.is_some()
        || !uob_contracts::valid_firmware_artifact_reference(&record.artifact_reference)
        || record.changed_at < record.admitted_at
        || record.last_status.is_some() != record.last_status_at.is_some()
        || record.last_status == Some(FirmwareStatus201::Idle)
        || (signature && !record.secure)
    {
        return Err(error());
    }
    Ok(FirmwareJobRecord201 {
        station: record.station,
        request_id: record.request_id,
        native_request_id: record.native_request_id,
        secure: record.secure,
        artifact_reference: record.artifact_reference,
        revision: record.revision,
        state: record.state,
        admitted_at: record.admitted_at,
        changed_at: record.changed_at,
        deadline: record.deadline,
        started: record.started,
        last_status: record.last_status,
        last_status_at: record.last_status_at,
        notifications: record.notifications,
        rejected_transitions: record.rejected_transitions,
    })
}
