use serde::{Deserialize, Serialize};
use uob_application::{FirmwareJobRecord16, FirmwareVariant16, StorageError, StorageErrorCode};
use uob_contracts::{FirmwareJobState16, FirmwareStatus16, RequestId, ResourceRef, UtcTimestamp};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    station: ResourceRef,
    request_id: RequestId,
    /// Present exactly for signed jobs.
    native_request_id: Option<i32>,
    artifact_reference: String,
    revision: u64,
    state: FirmwareJobState16,
    admitted_at: UtcTimestamp,
    changed_at: UtcTimestamp,
    deadline: UtcTimestamp,
    started: bool,
    last_status: Option<FirmwareStatus16>,
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

pub(crate) fn encode(record: &FirmwareJobRecord16) -> Result<String, StorageError> {
    serde_json::to_string(&Record {
        station: record.station.clone(),
        request_id: record.request_id.clone(),
        native_request_id: match record.variant {
            FirmwareVariant16::Legacy => None,
            FirmwareVariant16::Signed { request_id } => Some(request_id),
        },
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

pub(crate) fn decode(value: &str) -> Result<FirmwareJobRecord16, StorageError> {
    let record: Record = serde_json::from_str(value).map_err(|_| error())?;
    if record.revision == 0
        || record.station.resource.is_some()
        || !uob_contracts::valid_firmware_artifact_reference(&record.artifact_reference)
        || record.changed_at < record.admitted_at
        || record.last_status.is_some() != record.last_status_at.is_some()
        || record.last_status == Some(FirmwareStatus16::Idle)
        || (record.native_request_id.is_none()
            && record.last_status.is_some_and(|status| !status.legacy()))
    {
        return Err(error());
    }
    Ok(FirmwareJobRecord16 {
        station: record.station,
        request_id: record.request_id,
        variant: record
            .native_request_id
            .map_or(FirmwareVariant16::Legacy, |request_id| {
                FirmwareVariant16::Signed { request_id }
            }),
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
