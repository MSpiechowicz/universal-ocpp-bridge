#[path = "diagnostics201/migration.rs"]
mod migration;
#[path = "diagnostics201/recovery.rs"]
mod recovery;
#[path = "diagnostics201/support.rs"]
mod support;
use support::*;

#[tokio::test]
async fn job_holds_the_release_drain_until_the_provider_confirms_the_upload() {
    use LogUploadStatus201::{Uploaded, Uploading};
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "log", 7, 1).await.unwrap();
    assert_eq!(
        state(&store, &command).await,
        DiagnosticsJobState201::Pending
    );
    assert_eq!(database.release_jobs().len(), 1);
    assert_eq!(database.release_jobs()[0].1, "diagnostics");
    assert!(database.release_jobs()[0].0.starts_with("diagnostics201/"));
    dispatched(&store, &command, 2).await;
    assert_eq!(jobs(&store).await[0].upload_id, Some(upload_id(&command)));
    reply(&store, &command, status(GetLogStatus201::Accepted), 3).await;
    assert_eq!(
        state(&store, &command).await,
        DiagnosticsJobState201::Accepted
    );
    notify(&store, Uploading, Some(7), None, 10).await;
    assert_eq!(
        state(&store, &command).await,
        DiagnosticsJobState201::Uploading
    );
    assert_eq!(database.release_jobs().len(), 1);
    notify(&store, Uploaded, Some(7), Some(stored(&command)), 20).await;
    let evidence = evidence(&store, &command).await;
    assert_eq!(evidence.job.state, DiagnosticsJobState201::Uploaded);
    assert_eq!(evidence.job.notifications, 2);
    assert_eq!(evidence.job.last_status_at, Some(at(20)));
    assert_eq!(evidence.job.upload.as_ref().unwrap().size_bytes, 777);
    assert_eq!(evidence.reply, Some(status(GetLogStatus201::Accepted)));
    assert_eq!(evidence.destination, Some(destination()));
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
    notify(&store, Uploading, Some(7), None, 30).await;
    let settled = self::evidence(&store, &command).await;
    assert_eq!(settled.job.state, DiagnosticsJobState201::Uploaded);
    assert_eq!(settled.job.notifications, 3);
}

#[tokio::test]
async fn an_uploaded_claim_without_the_stored_file_is_unconfirmed() {
    let database = Database::new();
    let store = database.open();
    let first = admit(&store, "first", 1, 1).await.unwrap();
    dispatched(&store, &first, 2).await;
    reply(&store, &first, status(GetLogStatus201::Accepted), 3).await;
    notify(
        &store,
        LogUploadStatus201::Uploaded,
        Some(1),
        Some(UploadCheck201 {
            upload_id: upload_id(&first),
            outcome: UploadOutcome201::Missing,
        }),
        4,
    )
    .await;
    let evidence = evidence(&store, &first).await;
    assert_eq!(
        evidence.job.state,
        DiagnosticsJobState201::UploadUnconfirmed
    );
    assert!(evidence.job.upload.is_none());
    assert!(database.release_jobs().is_empty());
    // A provider fact for another destination cannot confirm this job.
    let second = admit(&store, "second", 2, 10).await.unwrap();
    dispatched(&store, &second, 11).await;
    reply(&store, &second, status(GetLogStatus201::Accepted), 12).await;
    notify(
        &store,
        LogUploadStatus201::Uploaded,
        Some(2),
        Some(stored(&first)),
        13,
    )
    .await;
    assert_eq!(
        state(&store, &second).await,
        DiagnosticsJobState201::UploadUnconfirmed
    );
}

#[tokio::test]
async fn single_flight_and_deadlines_never_release_the_drain() {
    let database = Database::new();
    let store = database.open();
    let first = admit(&store, "first", 1, 1).await.unwrap();
    // A second request while the first is still being dispatched is refused before admission.
    let conflict = admit(&store, "overlap", 2, 2).await.unwrap_err();
    assert_eq!(conflict.code(), StorageErrorCode::Conflict);
    assert!(
        store
            .command_by_request_id(RequestId::new("overlap").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    dispatched(&store, &first, 2).await;
    reply(&store, &first, status(GetLogStatus201::Accepted), 3).await;
    // The trusted deadline times the job out without resolving it.
    store.expire_diagnostics_jobs_201(at(700)).await.unwrap();
    assert_eq!(
        state(&store, &first).await,
        DiagnosticsJobState201::TimedOut
    );
    assert_eq!(database.release_jobs().len(), 1);
    // A plain acceptance of a newer request supersedes the job that nothing finished.
    let replacement = admit(&store, "replacement", 2, 701).await.unwrap();
    assert_eq!(database.release_jobs().len(), 2);
    dispatched(&store, &replacement, 702).await;
    reply(&store, &replacement, status(GetLogStatus201::Accepted), 703).await;
    assert_eq!(
        state(&store, &first).await,
        DiagnosticsJobState201::Superseded
    );
    assert_eq!(
        state(&store, &replacement).await,
        DiagnosticsJobState201::Accepted
    );
    assert_eq!(database.release_jobs().len(), 1);
}

#[tokio::test]
async fn notifications_match_only_their_request_id_and_cancellation_is_native() {
    use LogUploadStatus201::{AcceptedCanceled, UploadFailure, Uploaded, Uploading};
    let database = Database::new();
    let store = database.open();
    let first = admit(&store, "first", 7, 1).await.unwrap();
    dispatched(&store, &first, 2).await;
    reply(&store, &first, status(GetLogStatus201::Accepted), 3).await;
    notify(&store, Uploading, Some(8), None, 4).await;
    notify(&store, Uploading, None, None, 5).await;
    assert_eq!(
        state(&store, &first).await,
        DiagnosticsJobState201::Accepted
    );
    notify(&store, Uploading, Some(7), None, 6).await;
    assert_eq!(
        state(&store, &first).await,
        DiagnosticsJobState201::Uploading
    );
    assert_eq!(
        admit(&store, "reuse", 7, 7).await.unwrap_err().code(),
        StorageErrorCode::Conflict
    );
    // N01.FR.12: a new GetLog may be admitted while a log upload is in progress.
    let second = admit(&store, "second", 9, 8).await.unwrap();
    assert_eq!(database.release_jobs().len(), 2);
    dispatched(&store, &second, 9).await;
    reply(
        &store,
        &second,
        status(GetLogStatus201::AcceptedCanceled),
        10,
    )
    .await;
    assert_eq!(
        state(&store, &first).await,
        DiagnosticsJobState201::Cancelled
    );
    assert_eq!(database.release_jobs().len(), 1);
    // A late report for the cancelled upload is counted but never revives it.
    notify(&store, UploadFailure, Some(7), None, 11).await;
    let cancelled = evidence(&store, &first).await;
    assert_eq!(cancelled.job.state, DiagnosticsJobState201::Cancelled);
    assert_eq!(cancelled.job.notifications, 2);
    notify(&store, Uploaded, Some(9), Some(stored(&second)), 12).await;
    assert_eq!(
        state(&store, &second).await,
        DiagnosticsJobState201::Uploaded
    );
    assert!(database.release_jobs().is_empty());
    // N01.FR.20: the station's own AcceptedCanceled report cancels exactly its upload.
    let third = admit(&store, "third", 11, 20).await.unwrap();
    dispatched(&store, &third, 21).await;
    reply(&store, &third, status(GetLogStatus201::Accepted), 22).await;
    notify(&store, AcceptedCanceled, Some(11), None, 23).await;
    let cancelled = evidence(&store, &third).await;
    assert_eq!(cancelled.job.state, DiagnosticsJobState201::Cancelled);
    assert_eq!(
        cancelled.job.last_status,
        Some(LogUploadStatus201::AcceptedCanceled)
    );
    assert!(database.release_jobs().is_empty());
}

#[tokio::test]
async fn refusals_and_unsent_requests_release_without_progress() {
    let database = Database::new();
    let store = database.open();
    let rejected = admit(&store, "rejected", 1, 1).await.unwrap();
    dispatched(&store, &rejected, 2).await;
    reply(
        &store,
        &rejected,
        DiagnosticsReply201::Status {
            status: GetLogStatus201::Rejected,
            file_name: None,
            reason_code: Some("NoLogs".to_owned()),
        },
        3,
    )
    .await;
    let rejected_evidence = evidence(&store, &rejected).await;
    assert_eq!(
        rejected_evidence.job.state,
        DiagnosticsJobState201::Rejected
    );
    assert!(!rejected_evidence.accepted());
    let refused = admit(&store, "refused", 2, 10).await.unwrap();
    dispatched(&store, &refused, 11).await;
    reply(
        &store,
        &refused,
        DiagnosticsReply201::CallError {
            code: DiagnosticsCallError201::NotImplemented,
        },
        12,
    )
    .await;
    assert_eq!(
        state(&store, &refused).await,
        DiagnosticsJobState201::Rejected
    );
    let unsent = admit(&store, "unsent", 3, 30).await.unwrap();
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
    assert_eq!(
        state(&store, &unsent).await,
        DiagnosticsJobState201::NotSent
    );
    assert!(database.release_jobs().is_empty());
    // Every station end state releases the drain.
    for (index, status_value) in [
        LogUploadStatus201::BadMessage,
        LogUploadStatus201::PermissionDenied,
        LogUploadStatus201::NotSupportedOperation,
        LogUploadStatus201::UploadFailure,
    ]
    .into_iter()
    .enumerate()
    {
        let request_id = 100 + i32::try_from(index).unwrap();
        let now = 100 + i64::from(request_id);
        let command = admit(&store, &format!("failure-{index}"), request_id, now)
            .await
            .unwrap();
        dispatched(&store, &command, now + 1).await;
        reply(&store, &command, status(GetLogStatus201::Accepted), now + 2).await;
        notify(&store, status_value, Some(request_id), None, now + 3).await;
        assert!(state(&store, &command).await.resolved(), "{status_value:?}");
        assert!(database.release_jobs().is_empty(), "{status_value:?}");
    }
}

#[tokio::test]
async fn native_reply_destination_and_binding_are_immutable_and_must_match_the_request() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "log", 4, 1).await.unwrap();
    dispatched(&store, &command, 2).await;
    // A second binding, or one for a request that is not awaiting dispatch, is refused.
    assert!(
        store
            .bind_diagnostics_upload_201(command.request_id.clone(), "other".to_owned())
            .await
            .is_err()
    );
    assert!(
        store
            .bind_diagnostics_upload_201(RequestId::new("unknown").unwrap(), "x".to_owned())
            .await
            .is_err()
    );
    let mut other_request = reply_result(&command, status(GetLogStatus201::Accepted), 3);
    other_request.diagnostics_201.as_mut().unwrap().request_id = 5;
    assert!(persist(&store, other_request).await.is_err());
    let mut other_type = reply_result(&command, status(GetLogStatus201::Accepted), 3);
    other_type.diagnostics_201.as_mut().unwrap().log_type = LogType201::DiagnosticsLog;
    assert!(persist(&store, other_type).await.is_err());
    let mut wrong_destination = reply_result(&command, status(GetLogStatus201::Accepted), 3);
    wrong_destination
        .diagnostics_201
        .as_mut()
        .unwrap()
        .destination = Some(DiagnosticsDestination201 {
        log_type: LogType201::DiagnosticsLog,
        maximum_bytes: 4096,
        test_only: true,
    });
    assert!(persist(&store, wrong_destination).await.is_err());
    for reply in [
        DiagnosticsReply201::Status {
            status: GetLogStatus201::Accepted,
            file_name: Some("bad\u{7}".to_owned()),
            reason_code: None,
        },
        DiagnosticsReply201::Status {
            status: GetLogStatus201::Rejected,
            file_name: None,
            reason_code: Some("with space".to_owned()),
        },
    ] {
        assert!(
            persist(&store, reply_result(&command, reply, 3))
                .await
                .is_err()
        );
    }
    reply(&store, &command, status(GetLogStatus201::Accepted), 4).await;
    let changed = reply_result(&command, status(GetLogStatus201::Rejected), 5);
    assert!(persist(&store, changed).await.is_err());
    // A stale lower-rank writer keeps the settled reply and current job state.
    persist(&store, result(&command, CommandLifecycle::Dispatched, 6))
        .await
        .unwrap();
    let settled = evidence(&store, &command).await;
    assert!(settled.accepted());
    assert_eq!(settled.destination, Some(destination()));
    assert_eq!(settled.job.state, DiagnosticsJobState201::Accepted);
    assert!(
        store
            .bind_diagnostics_upload_201(command.request_id.clone(), "late".to_owned())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn identity_free_idle_settles_only_answered_jobs() {
    use LogUploadStatus201::{AcceptedCanceled, Idle, Uploading};
    let database = Database::new();
    let store = database.open();
    let log = admit(&store, "log", 3, 10).await.unwrap();
    dispatched(&store, &log, 11).await;
    // Unanswered jobs are not settled by an identity-free Idle (N01.FR.13).
    notify(&store, Idle, None, None, 12).await;
    assert_eq!(state(&store, &log).await, DiagnosticsJobState201::Pending);
    // Nor is any other identity-free status.
    reply(&store, &log, status(GetLogStatus201::Accepted), 13).await;
    notify(&store, AcceptedCanceled, None, None, 13).await;
    assert_eq!(state(&store, &log).await, DiagnosticsJobState201::Accepted);
    notify(&store, Uploading, Some(3), None, 14).await;
    notify(&store, Idle, None, None, 15).await;
    let idle = evidence(&store, &log).await;
    assert_eq!(idle.job.state, DiagnosticsJobState201::StationIdle);
    assert_eq!(idle.job.last_status, Some(Uploading));
    assert!(database.release_jobs().is_empty());
}

#[tokio::test]
async fn log_command_validation_binds_the_station_root_and_the_reference_schema() {
    let database = Database::new();
    let store = database.open();
    let mut wrong_schema = command("schema", 1, 1);
    if let CommandOperation::Ocpp(operation) = &mut wrong_schema.operation {
        operation.payload["remoteLocation"] = serde_json::json!("https://example.invalid/");
    }
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(wrong_schema);
    assert!(store.write_atomic(write).await.is_err());
    let mut scoped = command("scoped", 2, 1);
    scoped.resource.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
        evse_id: 1,
        connector_id: None,
    });
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(scoped);
    assert!(store.write_atomic(write).await.is_err());
}
