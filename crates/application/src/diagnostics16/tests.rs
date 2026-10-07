use super::{
    DiagnosticsJobRecord16, DiagnosticsTransition16, DiagnosticsVariant16, UploadCheck16,
    UploadOutcome16, apply_diagnostics_status_16, attribute_diagnostics_16,
};
use uob_contracts::{
    BridgeId, DiagnosticsJobState16, DiagnosticsUpload16, LogType16, LogUploadStatus16, RequestId,
    ResourceRef, StationId, UtcTimestamp,
};

fn at(seconds: i64) -> UtcTimestamp {
    UtcTimestamp::new(time::OffsetDateTime::from_unix_timestamp(1_800_000_000 + seconds).unwrap())
}

fn job(variant: DiagnosticsVariant16, revision: u64) -> DiagnosticsJobRecord16 {
    DiagnosticsJobRecord16 {
        station: ResourceRef {
            bridge_id: BridgeId::new("bridge").unwrap(),
            station_id: StationId::new("alpha").unwrap(),
            resource: None,
            native_protocol_reference: None,
        },
        request_id: RequestId::new(format!("diagnostics-{revision}")).unwrap(),
        variant,
        revision,
        state: DiagnosticsJobState16::Accepted,
        admitted_at: at(0),
        changed_at: at(0),
        deadline: at(3600),
        started: true,
        upload_id: Some(format!("upload-{revision}")),
        last_status: None,
        last_status_at: None,
        notifications: 0,
        rejected_transitions: 0,
        upload: None,
    }
}

fn log(request_id: i32, revision: u64) -> DiagnosticsJobRecord16 {
    job(
        DiagnosticsVariant16::Log {
            log_type: LogType16::SecurityLog,
            request_id,
        },
        revision,
    )
}

fn received(upload_id: &str) -> UploadCheck16 {
    UploadCheck16 {
        upload_id: upload_id.to_owned(),
        outcome: UploadOutcome16::Received(DiagnosticsUpload16 {
            sha256: "ab".repeat(32),
            size_bytes: 512,
        }),
    }
}

#[test]
fn uploaded_is_confirmed_only_by_the_jobs_own_stored_destination() {
    let mut record = job(DiagnosticsVariant16::Diagnostics, 1);
    assert_eq!(
        apply_diagnostics_status_16(&mut record, LogUploadStatus16::Uploading, None, at(1)),
        DiagnosticsTransition16::Advanced
    );
    assert_eq!(
        apply_diagnostics_status_16(
            &mut record,
            LogUploadStatus16::Uploaded,
            Some(&received("upload-1")),
            at(2)
        ),
        DiagnosticsTransition16::Advanced
    );
    assert_eq!(record.state, DiagnosticsJobState16::Uploaded);
    assert_eq!(record.upload.as_ref().unwrap().size_bytes, 512);
    assert_eq!(record.last_status, Some(LogUploadStatus16::Uploaded));
    assert_eq!(record.changed_at, at(2));

    for check in [
        None,
        Some(received("upload-other")),
        Some(UploadCheck16 {
            upload_id: "upload-1".to_owned(),
            outcome: UploadOutcome16::Missing,
        }),
    ] {
        let mut record = job(DiagnosticsVariant16::Diagnostics, 1);
        apply_diagnostics_status_16(
            &mut record,
            LogUploadStatus16::Uploaded,
            check.as_ref(),
            at(1),
        );
        assert_eq!(record.state, DiagnosticsJobState16::UploadUnconfirmed);
        assert!(record.upload.is_none());
    }
}

#[test]
fn failures_family_mismatches_and_late_facts_are_classified() {
    for (status, state) in [
        (
            LogUploadStatus16::UploadFailure,
            DiagnosticsJobState16::UploadFailed,
        ),
        (
            LogUploadStatus16::BadMessage,
            DiagnosticsJobState16::BadMessage,
        ),
        (
            LogUploadStatus16::NotSupportedOperation,
            DiagnosticsJobState16::NotSupportedOperation,
        ),
        (
            LogUploadStatus16::PermissionDenied,
            DiagnosticsJobState16::PermissionDenied,
        ),
        (LogUploadStatus16::Idle, DiagnosticsJobState16::StationIdle),
    ] {
        let mut record = log(7, 1);
        apply_diagnostics_status_16(&mut record, status, None, at(1));
        assert_eq!(record.state, state, "{status:?}");
        assert!(record.state.resolved());
    }
    let mut legacy = job(DiagnosticsVariant16::Diagnostics, 1);
    assert_eq!(
        apply_diagnostics_status_16(&mut legacy, LogUploadStatus16::BadMessage, None, at(1)),
        DiagnosticsTransition16::Rejected
    );
    assert_eq!(legacy.state, DiagnosticsJobState16::Accepted);
    assert_eq!(legacy.rejected_transitions, 1);
    apply_diagnostics_status_16(&mut legacy, LogUploadStatus16::UploadFailed, None, at(2));
    assert_eq!(legacy.state, DiagnosticsJobState16::UploadFailed);
    let mut security = log(7, 1);
    assert_eq!(
        apply_diagnostics_status_16(&mut security, LogUploadStatus16::UploadFailed, None, at(1)),
        DiagnosticsTransition16::Rejected
    );
    // A resolved job counts late facts but is never revived.
    assert_eq!(
        apply_diagnostics_status_16(&mut legacy, LogUploadStatus16::Uploading, None, at(3)),
        DiagnosticsTransition16::Late
    );
    assert_eq!(legacy.state, DiagnosticsJobState16::UploadFailed);
    assert_eq!(legacy.notifications, 3);
    // An idle report resolves the job without pretending to know the outcome.
    let mut idle = job(DiagnosticsVariant16::Diagnostics, 1);
    apply_diagnostics_status_16(&mut idle, LogUploadStatus16::Uploading, None, at(1));
    apply_diagnostics_status_16(&mut idle, LogUploadStatus16::Idle, None, at(2));
    assert_eq!(idle.state, DiagnosticsJobState16::StationIdle);
    assert_eq!(idle.last_status, Some(LogUploadStatus16::Uploading));
}

#[test]
fn notifications_are_attributed_by_family_and_exact_request_id() {
    let mut older_log = log(7, 1);
    older_log.state = DiagnosticsJobState16::Uploaded;
    let mut pending_log = log(9, 4);
    pending_log.state = DiagnosticsJobState16::Pending;
    let records = vec![
        older_log,
        job(DiagnosticsVariant16::Diagnostics, 2),
        log(8, 3),
        pending_log,
    ];
    let legacy = |status| attribute_diagnostics_16(&records, status, false, None);
    assert_eq!(legacy(LogUploadStatus16::Uploading), Some(1));
    assert_eq!(
        attribute_diagnostics_16(&records, LogUploadStatus16::Uploaded, true, Some(7)),
        Some(0),
        "a resolved job still owns its requestId so the late fact is counted"
    );
    assert_eq!(
        attribute_diagnostics_16(&records, LogUploadStatus16::Uploaded, true, Some(8)),
        Some(2)
    );
    assert_eq!(
        attribute_diagnostics_16(&records, LogUploadStatus16::Uploaded, true, Some(99)),
        None
    );
    // Identity-free Idle skips the unanswered newest job (N01.FR.12).
    assert_eq!(
        attribute_diagnostics_16(&records, LogUploadStatus16::Idle, true, None),
        Some(2)
    );
    assert_eq!(
        attribute_diagnostics_16(&records, LogUploadStatus16::Uploading, true, None),
        None
    );
    let only_resolved = vec![{
        let mut record = job(DiagnosticsVariant16::Diagnostics, 1);
        record.state = DiagnosticsJobState16::NoLogAvailable;
        record
    }];
    assert_eq!(
        attribute_diagnostics_16(&only_resolved, LogUploadStatus16::Uploading, false, None),
        None
    );
}
