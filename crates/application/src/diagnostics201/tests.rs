use super::{
    DiagnosticsJobRecord201, DiagnosticsTransition201, UploadCheck201, UploadOutcome201,
    apply_diagnostics_status_201, attribute_diagnostics_201,
};
use uob_contracts::{
    BridgeId, DiagnosticsJobState201, DiagnosticsUpload201, LogType201, LogUploadStatus201,
    RequestId, ResourceRef, StationId, UtcTimestamp,
};

fn at(seconds: i64) -> UtcTimestamp {
    UtcTimestamp::new(time::OffsetDateTime::from_unix_timestamp(1_800_000_000 + seconds).unwrap())
}

fn log(request_id: i32, revision: u64) -> DiagnosticsJobRecord201 {
    DiagnosticsJobRecord201 {
        station: ResourceRef {
            bridge_id: BridgeId::new("bridge").unwrap(),
            station_id: StationId::new("alpha").unwrap(),
            resource: None,
            native_protocol_reference: None,
        },
        request_id: RequestId::new(format!("log-{revision}")).unwrap(),
        native_request_id: request_id,
        log_type: LogType201::SecurityLog,
        revision,
        state: DiagnosticsJobState201::Accepted,
        admitted_at: at(0),
        changed_at: at(0),
        deadline: at(3600),
        started: true,
        upload_id: Some(format!("upload-{revision}")),
        last_status: None,
        last_status_at: None,
        notifications: 0,
        upload: None,
    }
}

fn received(upload_id: &str) -> UploadCheck201 {
    UploadCheck201 {
        upload_id: upload_id.to_owned(),
        outcome: UploadOutcome201::Received(DiagnosticsUpload201 {
            sha256: "ab".repeat(32),
            size_bytes: 512,
        }),
    }
}

#[test]
fn uploaded_is_confirmed_only_by_the_jobs_own_stored_destination() {
    let mut record = log(7, 1);
    assert_eq!(
        apply_diagnostics_status_201(&mut record, LogUploadStatus201::Uploading, None, at(1)),
        DiagnosticsTransition201::Advanced
    );
    assert_eq!(
        apply_diagnostics_status_201(
            &mut record,
            LogUploadStatus201::Uploaded,
            Some(&received("upload-1")),
            at(2)
        ),
        DiagnosticsTransition201::Advanced
    );
    assert_eq!(record.state, DiagnosticsJobState201::Uploaded);
    assert_eq!(record.upload.as_ref().unwrap().size_bytes, 512);
    assert_eq!(record.last_status, Some(LogUploadStatus201::Uploaded));
    assert_eq!(record.changed_at, at(2));

    for check in [
        None,
        Some(received("upload-other")),
        Some(UploadCheck201 {
            upload_id: "upload-1".to_owned(),
            outcome: UploadOutcome201::Missing,
        }),
    ] {
        let mut record = log(7, 1);
        apply_diagnostics_status_201(
            &mut record,
            LogUploadStatus201::Uploaded,
            check.as_ref(),
            at(1),
        );
        assert_eq!(record.state, DiagnosticsJobState201::UploadUnconfirmed);
        assert!(record.upload.is_none());
    }
}

#[test]
fn every_native_end_state_and_late_fact_is_classified() {
    for (status, state) in [
        (
            LogUploadStatus201::UploadFailure,
            DiagnosticsJobState201::UploadFailed,
        ),
        (
            LogUploadStatus201::BadMessage,
            DiagnosticsJobState201::BadMessage,
        ),
        (
            LogUploadStatus201::NotSupportedOperation,
            DiagnosticsJobState201::NotSupportedOperation,
        ),
        (
            LogUploadStatus201::PermissionDenied,
            DiagnosticsJobState201::PermissionDenied,
        ),
        (
            LogUploadStatus201::AcceptedCanceled,
            DiagnosticsJobState201::Cancelled,
        ),
        (
            LogUploadStatus201::Idle,
            DiagnosticsJobState201::StationIdle,
        ),
    ] {
        let mut record = log(7, 1);
        apply_diagnostics_status_201(&mut record, status, None, at(1));
        assert_eq!(record.state, state, "{status:?}");
        assert!(record.state.resolved());
    }
    // A resolved job counts late facts but is never revived.
    let mut record = log(7, 1);
    apply_diagnostics_status_201(&mut record, LogUploadStatus201::UploadFailure, None, at(1));
    assert_eq!(
        apply_diagnostics_status_201(&mut record, LogUploadStatus201::Uploading, None, at(2)),
        DiagnosticsTransition201::Late
    );
    assert_eq!(record.state, DiagnosticsJobState201::UploadFailed);
    assert_eq!(record.notifications, 2);
    assert_eq!(record.changed_at, at(1));
    // An idle report resolves the job without pretending to know the outcome.
    let mut idle = log(7, 1);
    apply_diagnostics_status_201(&mut idle, LogUploadStatus201::Uploading, None, at(1));
    apply_diagnostics_status_201(&mut idle, LogUploadStatus201::Idle, None, at(2));
    assert_eq!(idle.state, DiagnosticsJobState201::StationIdle);
    assert_eq!(idle.last_status, Some(LogUploadStatus201::Uploading));
}

#[test]
fn notifications_are_attributed_only_by_exact_request_id() {
    let mut older = log(7, 1);
    older.state = DiagnosticsJobState201::Uploaded;
    let mut pending = log(9, 3);
    pending.state = DiagnosticsJobState201::Pending;
    let records = vec![older, log(8, 2), pending];
    let attribute = |status, request_id| attribute_diagnostics_201(&records, status, request_id);
    assert_eq!(
        attribute(LogUploadStatus201::Uploaded, Some(7)),
        Some(0),
        "a resolved job still owns its requestId so the late fact is counted"
    );
    assert_eq!(attribute(LogUploadStatus201::Uploaded, Some(8)), Some(1));
    assert_eq!(attribute(LogUploadStatus201::Uploading, Some(9)), Some(2));
    assert_eq!(attribute(LogUploadStatus201::Uploaded, Some(99)), None);
    // Identity-free Idle skips the unanswered newest job (N01.FR.13).
    assert_eq!(attribute(LogUploadStatus201::Idle, None), Some(1));
    for status in [
        LogUploadStatus201::Uploading,
        LogUploadStatus201::Uploaded,
        LogUploadStatus201::AcceptedCanceled,
    ] {
        assert_eq!(attribute(status, None), None, "{status:?}");
    }
    let only_resolved = vec![{
        let mut record = log(7, 1);
        record.state = DiagnosticsJobState201::Rejected;
        record
    }];
    assert_eq!(
        attribute_diagnostics_201(&only_resolved, LogUploadStatus201::Idle, None),
        None
    );
}
