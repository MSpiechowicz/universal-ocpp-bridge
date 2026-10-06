#[path = "firmware16/migration.rs"]
mod migration;
#[path = "firmware16/recovery.rs"]
mod recovery;
#[path = "firmware16/support.rs"]
mod support;
use support::*;

#[tokio::test]
async fn legacy_job_holds_the_release_drain_until_native_installation_is_reported() {
    use FirmwareStatus16::{Downloaded, Downloading, Installed, Installing};
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "legacy", FirmwareVariant16::Legacy, 1)
        .await
        .unwrap();
    assert_eq!(state(&store, &command).await, FirmwareJobState16::Pending);
    assert_eq!(database.release_jobs().len(), 1);
    assert_eq!(database.release_jobs()[0].1, "firmware");
    assert!(database.release_jobs()[0].0.starts_with("firmware16/"));
    dispatched(&store, &command, 2).await;
    reply(&store, &command, FirmwareReply16::Acknowledged, 3).await;
    assert_eq!(state(&store, &command).await, FirmwareJobState16::Accepted);
    for (offset, status) in [Downloading, Downloaded, Installing]
        .into_iter()
        .enumerate()
    {
        notify(
            &store,
            status,
            false,
            None,
            10 + i64::try_from(offset).unwrap(),
        )
        .await;
        assert_eq!(database.release_jobs().len(), 1, "{status:?}");
    }
    assert_eq!(
        state(&store, &command).await,
        FirmwareJobState16::Installing
    );
    notify(&store, Installed, false, None, 20).await;
    let evidence = evidence(&store, &command).await;
    assert_eq!(evidence.job().state, FirmwareJobState16::Installed);
    assert_eq!(evidence.job().notifications, 4);
    assert_eq!(evidence.job().last_status_at, Some(at(20)));
    assert_eq!(evidence.reply(), Some(FirmwareReply16::Acknowledged));
    assert_eq!(evidence.artifact(), Some(&artifact(false)));
    assert!(database.release_jobs().is_empty());
    // A late duplicate never reopens or changes the settled job.
    notify(&store, Downloading, false, None, 30).await;
    assert_eq!(state(&store, &command).await, FirmwareJobState16::Installed);
}

#[tokio::test]
async fn single_flight_and_active_legacy_jobs_refuse_new_requests_before_admission() {
    let database = Database::new();
    let store = database.open();
    let first = admit(&store, "first", FirmwareVariant16::Legacy, 1)
        .await
        .unwrap();
    let conflict = admit(&store, "overlap", FirmwareVariant16::Legacy, 2)
        .await
        .unwrap_err();
    assert_eq!(conflict.code(), StorageErrorCode::Conflict);
    assert!(
        store
            .command_by_request_id(RequestId::new("overlap").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    dispatched(&store, &first, 2).await;
    reply(&store, &first, FirmwareReply16::Acknowledged, 3).await;
    assert_eq!(
        admit(&store, "active", FirmwareVariant16::Legacy, 4)
            .await
            .unwrap_err()
            .code(),
        StorageErrorCode::Conflict
    );
    // The trusted deadline times the job out without resolving it.
    store.expire_firmware_jobs_16(at(700)).await.unwrap();
    assert_eq!(state(&store, &first).await, FirmwareJobState16::TimedOut);
    assert_eq!(database.release_jobs().len(), 1);
    let replacement = admit(&store, "replacement", FirmwareVariant16::Legacy, 701)
        .await
        .unwrap();
    assert_eq!(database.release_jobs().len(), 2);
    dispatched(&store, &replacement, 702).await;
    reply(&store, &replacement, FirmwareReply16::Acknowledged, 703).await;
    assert_eq!(state(&store, &first).await, FirmwareJobState16::Superseded);
    assert_eq!(
        state(&store, &replacement).await,
        FirmwareJobState16::Accepted
    );
    assert_eq!(database.release_jobs().len(), 1);
}

#[tokio::test]
async fn signed_notifications_match_only_their_request_id_and_cancellation_is_native() {
    use FirmwareStatus16::{Downloading, InstallationFailed, SignatureVerified};
    let database = Database::new();
    let store = database.open();
    let first = admit(
        &store,
        "first",
        FirmwareVariant16::Signed { request_id: 7 },
        1,
    )
    .await
    .unwrap();
    dispatched(&store, &first, 2).await;
    reply(
        &store,
        &first,
        signed_status(SignedUpdateFirmwareStatus16::Accepted),
        3,
    )
    .await;
    notify(&store, Downloading, true, Some(8), 4).await;
    notify(&store, Downloading, false, None, 5).await;
    assert_eq!(state(&store, &first).await, FirmwareJobState16::Accepted);
    notify(&store, Downloading, true, Some(7), 6).await;
    assert_eq!(state(&store, &first).await, FirmwareJobState16::Downloading);
    assert_eq!(
        admit(
            &store,
            "reuse",
            FirmwareVariant16::Signed { request_id: 7 },
            7
        )
        .await
        .unwrap_err()
        .code(),
        StorageErrorCode::Conflict
    );
    let second = admit(
        &store,
        "second",
        FirmwareVariant16::Signed { request_id: 9 },
        8,
    )
    .await
    .unwrap();
    assert_eq!(database.release_jobs().len(), 2);
    dispatched(&store, &second, 9).await;
    reply(
        &store,
        &second,
        signed_status(SignedUpdateFirmwareStatus16::AcceptedCanceled),
        10,
    )
    .await;
    assert_eq!(state(&store, &first).await, FirmwareJobState16::Cancelled);
    assert_eq!(database.release_jobs().len(), 1);
    // L01.FR.26 allows a failure report for the cancelled update; it never revives it.
    notify(&store, InstallationFailed, true, Some(7), 11).await;
    let cancelled = evidence(&store, &first).await;
    assert_eq!(cancelled.job().state, FirmwareJobState16::Cancelled);
    assert_eq!(cancelled.job().notifications, 2);
    notify(&store, SignatureVerified, true, Some(9), 12).await;
    assert_eq!(
        state(&store, &second).await,
        FirmwareJobState16::SignatureVerified
    );
}

#[tokio::test]
async fn refusals_and_unsent_requests_release_without_progress() {
    let database = Database::new();
    let store = database.open();
    for (index, reply_value) in [
        signed_status(SignedUpdateFirmwareStatus16::Rejected),
        signed_status(SignedUpdateFirmwareStatus16::InvalidCertificate),
        signed_status(SignedUpdateFirmwareStatus16::RevokedCertificate),
    ]
    .into_iter()
    .enumerate()
    {
        let id = i32::try_from(index).unwrap();
        let now = i64::from(id) * 10 + 1;
        let command = admit(
            &store,
            &format!("signed-{id}"),
            FirmwareVariant16::Signed { request_id: id },
            now,
        )
        .await
        .unwrap();
        dispatched(&store, &command, now + 1).await;
        reply(&store, &command, reply_value, now + 2).await;
        let evidence = evidence(&store, &command).await;
        assert_eq!(evidence.job().state, FirmwareJobState16::Rejected);
        assert_eq!(evidence.reply(), Some(reply_value));
        assert!(database.release_jobs().is_empty());
    }
    let legacy = admit(&store, "legacy", FirmwareVariant16::Legacy, 100)
        .await
        .unwrap();
    dispatched(&store, &legacy, 101).await;
    reply(
        &store,
        &legacy,
        FirmwareReply16::CallError {
            code: FirmwareCallError16::NotSupported,
        },
        102,
    )
    .await;
    assert_eq!(state(&store, &legacy).await, FirmwareJobState16::Rejected);
    let unsent = admit(&store, "unsent", FirmwareVariant16::Legacy, 110)
        .await
        .unwrap();
    persist(
        &store,
        result(
            &unsent,
            CommandLifecycle::Rejected {
                error: CommandError {
                    code: CommandErrorCode::PolicyRejected,
                    detail: Some("firmware artifact unavailable".to_owned()),
                },
            },
            111,
        ),
    )
    .await
    .unwrap();
    assert_eq!(state(&store, &unsent).await, FirmwareJobState16::NotSent);
    assert!(database.release_jobs().is_empty());
}

#[tokio::test]
async fn native_reply_and_artifact_are_immutable_and_must_match_the_request() {
    let database = Database::new();
    let store = database.open();
    let command = admit(
        &store,
        "signed",
        FirmwareVariant16::Signed { request_id: 4 },
        1,
    )
    .await
    .unwrap();
    dispatched(&store, &command, 2).await;
    let mut legacy_reply = reply_result(
        &command,
        signed_status(SignedUpdateFirmwareStatus16::Accepted),
        3,
    );
    if let Some(FirmwareResult16::SignedUpdateFirmware { request_id, .. }) =
        &mut legacy_reply.firmware_16
    {
        *request_id = 5;
    }
    assert!(persist(&store, legacy_reply).await.is_err());
    let mut wrong_shape = reply_result(&command, FirmwareReply16::Acknowledged, 3);
    wrong_shape.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted: true,
        error: None,
    };
    assert!(persist(&store, wrong_shape).await.is_err());
    reply(
        &store,
        &command,
        signed_status(SignedUpdateFirmwareStatus16::Accepted),
        4,
    )
    .await;
    let changed = reply_result(
        &command,
        signed_status(SignedUpdateFirmwareStatus16::Rejected),
        5,
    );
    assert!(persist(&store, changed).await.is_err());
    // A stale lower-rank writer keeps the settled reply and current job state.
    dispatched(&store, &command, 6).await;
    let settled = evidence(&store, &command).await;
    assert!(settled.accepted());
    assert_eq!(settled.artifact(), Some(&artifact(true)));
    assert_eq!(settled.job().state, FirmwareJobState16::Accepted);
}

#[tokio::test]
async fn regressions_are_counted_and_identity_free_idle_resolves_signed_jobs() {
    use FirmwareStatus16::{Downloaded, Downloading, Idle};
    let database = Database::new();
    let store = database.open();
    let command = admit(
        &store,
        "signed",
        FirmwareVariant16::Signed { request_id: 1 },
        1,
    )
    .await
    .unwrap();
    dispatched(&store, &command, 2).await;
    reply(
        &store,
        &command,
        signed_status(SignedUpdateFirmwareStatus16::Accepted),
        3,
    )
    .await;
    notify(&store, Downloaded, true, Some(1), 4).await;
    notify(&store, Downloading, true, Some(1), 5).await;
    let regressed = evidence(&store, &command).await;
    assert_eq!(regressed.job().state, FirmwareJobState16::Downloaded);
    assert_eq!(regressed.job().rejected_transitions, 1);
    assert_eq!(regressed.job().notifications, 2);
    notify(&store, Downloading, true, None, 6).await;
    assert_eq!(
        state(&store, &command).await,
        FirmwareJobState16::Downloaded
    );
    notify(&store, Idle, true, None, 7).await;
    assert_eq!(
        state(&store, &command).await,
        FirmwareJobState16::StationIdle
    );
    assert!(database.release_jobs().is_empty());
}
