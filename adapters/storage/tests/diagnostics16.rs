#[path = "diagnostics16/migration.rs"]
mod migration;
#[path = "diagnostics16/recovery.rs"]
mod recovery;
#[path = "diagnostics16/support.rs"]
mod support;
use support::*;

#[tokio::test]
async fn legacy_job_holds_the_release_drain_until_the_provider_confirms_the_upload() {
    use LogUploadStatus16::{Uploaded, Uploading};
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "legacy", LEGACY, 1).await.unwrap();
    assert_eq!(
        state(&store, &command).await,
        DiagnosticsJobState16::Pending
    );
    assert_eq!(database.release_jobs().len(), 1);
    assert_eq!(database.release_jobs()[0].1, "diagnostics");
    assert!(database.release_jobs()[0].0.starts_with("diagnostics16/"));
    dispatched(&store, &command, 2).await;
    assert_eq!(jobs(&store).await[0].upload_id, Some(upload_id(&command)));
    reply(&store, &command, file("diag.zip"), 3).await;
    assert_eq!(
        state(&store, &command).await,
        DiagnosticsJobState16::Accepted
    );
    notify(&store, Uploading, false, None, None, 10).await;
    assert_eq!(
        state(&store, &command).await,
        DiagnosticsJobState16::Uploading
    );
    assert_eq!(database.release_jobs().len(), 1);
    notify(&store, Uploaded, false, None, Some(stored(&command)), 20).await;
    let evidence = evidence(&store, &command).await;
    assert_eq!(evidence.job().state, DiagnosticsJobState16::Uploaded);
    assert_eq!(evidence.job().notifications, 2);
    assert_eq!(evidence.job().last_status_at, Some(at(20)));
    assert_eq!(evidence.job().upload.as_ref().unwrap().size_bytes, 777);
    assert_eq!(evidence.reply(), Some(&file("diag.zip")));
    assert_eq!(evidence.destination(), Some(&destination(LEGACY)));
    assert!(database.release_jobs().is_empty());
    // The private destination identity never reaches the public result.
    let public = serde_json::to_string(
        &store
            .command_result_by_request_id(command.request_id.clone())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(!public.contains(&upload_id(&command)));
    // A late duplicate never reopens or changes the settled job.
    notify(&store, Uploading, false, None, None, 30).await;
    assert_eq!(
        state(&store, &command).await,
        DiagnosticsJobState16::Uploaded
    );
}

#[tokio::test]
async fn an_uploaded_claim_without_the_stored_file_is_unconfirmed() {
    let database = Database::new();
    let store = database.open();
    let first = admit(&store, "first", LEGACY, 1).await.unwrap();
    dispatched(&store, &first, 2).await;
    reply(&store, &first, file("a.log"), 3).await;
    notify(
        &store,
        LogUploadStatus16::Uploaded,
        false,
        None,
        Some(UploadCheck16 {
            upload_id: upload_id(&first),
            outcome: UploadOutcome16::Missing,
        }),
        4,
    )
    .await;
    let evidence = evidence(&store, &first).await;
    assert_eq!(
        evidence.job().state,
        DiagnosticsJobState16::UploadUnconfirmed
    );
    assert!(evidence.job().upload.is_none());
    assert!(database.release_jobs().is_empty());
    // A provider fact for another destination cannot confirm this job.
    let second = admit(&store, "second", LEGACY, 10).await.unwrap();
    dispatched(&store, &second, 11).await;
    reply(&store, &second, file("b.log"), 12).await;
    notify(
        &store,
        LogUploadStatus16::Uploaded,
        false,
        None,
        Some(stored(&first)),
        13,
    )
    .await;
    assert_eq!(
        state(&store, &second).await,
        DiagnosticsJobState16::UploadUnconfirmed
    );
}

#[tokio::test]
async fn single_flight_and_active_legacy_uploads_refuse_new_requests_before_admission() {
    let database = Database::new();
    let store = database.open();
    let first = admit(&store, "first", LEGACY, 1).await.unwrap();
    let conflict = admit(&store, "overlap", security(1), 2).await.unwrap_err();
    assert_eq!(conflict.code(), StorageErrorCode::Conflict);
    assert!(
        store
            .command_by_request_id(RequestId::new("overlap").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    dispatched(&store, &first, 2).await;
    reply(&store, &first, file("a.log"), 3).await;
    for (id, variant) in [("active", LEGACY), ("log-over-legacy", security(2))] {
        assert_eq!(
            admit(&store, id, variant, 4).await.unwrap_err().code(),
            StorageErrorCode::Conflict,
            "{id}"
        );
    }
    // The trusted deadline times the job out without resolving it.
    store.expire_diagnostics_jobs_16(at(700)).await.unwrap();
    assert_eq!(state(&store, &first).await, DiagnosticsJobState16::TimedOut);
    assert_eq!(database.release_jobs().len(), 1);
    let replacement = admit(&store, "replacement", LEGACY, 701).await.unwrap();
    assert_eq!(database.release_jobs().len(), 2);
    dispatched(&store, &replacement, 702).await;
    reply(&store, &replacement, file("b.log"), 703).await;
    assert_eq!(
        state(&store, &first).await,
        DiagnosticsJobState16::Superseded
    );
    assert_eq!(
        state(&store, &replacement).await,
        DiagnosticsJobState16::Accepted
    );
    assert_eq!(database.release_jobs().len(), 1);
}

#[tokio::test]
async fn log_notifications_match_only_their_request_id_and_cancellation_is_native() {
    use LogUploadStatus16::{UploadFailure, Uploaded, Uploading};
    let database = Database::new();
    let store = database.open();
    let first = admit(&store, "first", security(7), 1).await.unwrap();
    dispatched(&store, &first, 2).await;
    reply(&store, &first, log_status(GetLogStatus16::Accepted), 3).await;
    notify(&store, Uploading, true, Some(8), None, 4).await;
    notify(&store, Uploading, false, None, None, 5).await;
    assert_eq!(state(&store, &first).await, DiagnosticsJobState16::Accepted);
    notify(&store, Uploading, true, Some(7), None, 6).await;
    assert_eq!(
        state(&store, &first).await,
        DiagnosticsJobState16::Uploading
    );
    assert_eq!(
        admit(&store, "reuse", security(7), 7)
            .await
            .unwrap_err()
            .code(),
        StorageErrorCode::Conflict
    );
    // N01.FR.11: a new GetLog may be admitted while a log upload is in progress.
    let second = admit(&store, "second", security(9), 8).await.unwrap();
    assert_eq!(database.release_jobs().len(), 2);
    dispatched(&store, &second, 9).await;
    reply(
        &store,
        &second,
        log_status(GetLogStatus16::AcceptedCanceled),
        10,
    )
    .await;
    assert_eq!(
        state(&store, &first).await,
        DiagnosticsJobState16::Cancelled
    );
    assert_eq!(database.release_jobs().len(), 1);
    // A late report for the cancelled upload is counted but never revives it.
    notify(&store, UploadFailure, true, Some(7), None, 11).await;
    let cancelled = evidence(&store, &first).await;
    assert_eq!(cancelled.job().state, DiagnosticsJobState16::Cancelled);
    assert_eq!(cancelled.job().notifications, 2);
    notify(&store, Uploaded, true, Some(9), Some(stored(&second)), 12).await;
    assert_eq!(
        state(&store, &second).await,
        DiagnosticsJobState16::Uploaded
    );
    assert!(database.release_jobs().is_empty());
}

#[tokio::test]
async fn refusals_no_file_and_unsent_requests_release_without_progress() {
    let database = Database::new();
    let store = database.open();
    let rejected = admit(&store, "rejected", security(1), 1).await.unwrap();
    dispatched(&store, &rejected, 2).await;
    reply(&store, &rejected, log_status(GetLogStatus16::Rejected), 3).await;
    assert_eq!(
        state(&store, &rejected).await,
        DiagnosticsJobState16::Rejected
    );
    let refused = admit(&store, "refused", LEGACY, 10).await.unwrap();
    dispatched(&store, &refused, 11).await;
    reply(
        &store,
        &refused,
        DiagnosticsReply16::CallError {
            code: DiagnosticsCallError16::NotImplemented,
        },
        12,
    )
    .await;
    assert_eq!(
        state(&store, &refused).await,
        DiagnosticsJobState16::Rejected
    );
    let empty = admit(&store, "empty", LEGACY, 20).await.unwrap();
    dispatched(&store, &empty, 21).await;
    reply(
        &store,
        &empty,
        DiagnosticsReply16::Diagnostics { file_name: None },
        22,
    )
    .await;
    let evidence = evidence(&store, &empty).await;
    assert_eq!(evidence.job().state, DiagnosticsJobState16::NoLogAvailable);
    assert!(!evidence.accepted());
    let unsent = admit(&store, "unsent", LEGACY, 30).await.unwrap();
    persist(
        &store,
        result(
            &unsent,
            CommandLifecycle::Rejected {
                error: CommandError {
                    code: CommandErrorCode::PolicyRejected,
                    detail: Some("log upload destination unavailable".to_owned()),
                },
            },
            31,
        ),
    )
    .await
    .unwrap();
    assert_eq!(state(&store, &unsent).await, DiagnosticsJobState16::NotSent);
    assert!(database.release_jobs().is_empty());
    // Every station end state releases the drain.
    for (index, status) in [
        LogUploadStatus16::BadMessage,
        LogUploadStatus16::PermissionDenied,
        LogUploadStatus16::NotSupportedOperation,
    ]
    .into_iter()
    .enumerate()
    {
        let request_id = 100 + i32::try_from(index).unwrap();
        let now = 100 + i64::from(request_id);
        let command = admit(
            &store,
            &format!("failure-{index}"),
            security(request_id),
            now,
        )
        .await
        .unwrap();
        dispatched(&store, &command, now + 1).await;
        reply(
            &store,
            &command,
            log_status(GetLogStatus16::Accepted),
            now + 2,
        )
        .await;
        notify(&store, status, true, Some(request_id), None, now + 3).await;
        assert!(state(&store, &command).await.resolved(), "{status:?}");
        assert!(database.release_jobs().is_empty(), "{status:?}");
    }
}

#[tokio::test]
async fn native_reply_destination_and_binding_are_immutable_and_must_match_the_request() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "log", security(4), 1).await.unwrap();
    dispatched(&store, &command, 2).await;
    // A second binding, or one for a request that is not awaiting dispatch, is refused.
    assert!(
        store
            .bind_diagnostics_upload_16(command.request_id.clone(), "other".to_owned())
            .await
            .is_err()
    );
    assert!(
        store
            .bind_diagnostics_upload_16(RequestId::new("unknown").unwrap(), "x".to_owned())
            .await
            .is_err()
    );
    let mut other_request = reply_result(&command, log_status(GetLogStatus16::Accepted), 3);
    if let Some(DiagnosticsResult16::GetLog { request_id, .. }) = &mut other_request.diagnostics_16
    {
        *request_id = 5;
    }
    assert!(persist(&store, other_request).await.is_err());
    let wrong_shape = reply_result(&command, file("x.log"), 3);
    assert!(persist(&store, wrong_shape).await.is_err());
    let mut wrong_destination = reply_result(&command, log_status(GetLogStatus16::Accepted), 3);
    if let Some(DiagnosticsResult16::GetLog { destination, .. }) =
        &mut wrong_destination.diagnostics_16
    {
        *destination = Some(support::destination(LEGACY));
    }
    assert!(persist(&store, wrong_destination).await.is_err());
    let unprintable = reply_result(
        &command,
        DiagnosticsReply16::Log {
            status: GetLogStatus16::Accepted,
            file_name: Some("bad\u{7}".to_owned()),
        },
        3,
    );
    assert!(persist(&store, unprintable).await.is_err());
    reply(&store, &command, log_status(GetLogStatus16::Accepted), 4).await;
    let changed = reply_result(&command, log_status(GetLogStatus16::Rejected), 5);
    assert!(persist(&store, changed).await.is_err());
    // A stale lower-rank writer keeps the settled reply and current job state.
    persist(&store, result(&command, CommandLifecycle::Dispatched, 6))
        .await
        .unwrap();
    let settled = evidence(&store, &command).await;
    assert!(settled.accepted());
    assert_eq!(settled.destination(), Some(&destination(security(4))));
    assert_eq!(settled.job().state, DiagnosticsJobState16::Accepted);
    assert!(
        store
            .bind_diagnostics_upload_16(command.request_id.clone(), "late".to_owned())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn family_mismatches_are_counted_and_identity_free_idle_settles_answered_log_jobs() {
    use LogUploadStatus16::{BadMessage, Idle, Uploading};
    let database = Database::new();
    let store = database.open();
    let legacy = admit(&store, "legacy", LEGACY, 1).await.unwrap();
    dispatched(&store, &legacy, 2).await;
    reply(&store, &legacy, file("a.log"), 3).await;
    // A log-only value cannot arrive in a legacy notification; the decoder refuses it, and
    // storage still refuses to apply it to a legacy job.
    notify(&store, BadMessage, false, None, None, 4).await;
    let mismatch = evidence(&store, &legacy).await;
    assert_eq!(mismatch.job().state, DiagnosticsJobState16::Accepted);
    assert_eq!(mismatch.job().rejected_transitions, 1);
    notify(&store, Idle, false, None, None, 5).await;
    assert_eq!(
        state(&store, &legacy).await,
        DiagnosticsJobState16::StationIdle
    );

    let log = admit(&store, "log", security(3), 10).await.unwrap();
    dispatched(&store, &log, 11).await;
    // Unanswered jobs are not settled by an identity-free Idle.
    notify(&store, Idle, true, None, None, 12).await;
    assert_eq!(state(&store, &log).await, DiagnosticsJobState16::Pending);
    reply(&store, &log, log_status(GetLogStatus16::Accepted), 13).await;
    notify(&store, Uploading, true, Some(3), None, 14).await;
    notify(&store, Idle, true, None, None, 15).await;
    assert_eq!(
        state(&store, &log).await,
        DiagnosticsJobState16::StationIdle
    );
    assert!(database.release_jobs().is_empty());
}
