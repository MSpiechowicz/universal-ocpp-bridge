#[path = "firmware201/migration.rs"]
mod migration;
#[path = "firmware201/recovery.rs"]
mod recovery;
#[path = "firmware201/support.rs"]
mod support;
use support::*;

#[tokio::test]
async fn secure_job_holds_the_release_drain_until_native_installation_is_reported() {
    use FirmwareStatus201::{
        Downloaded, Downloading, InstallRebooting, Installed, Installing, SignatureVerified,
    };
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "secure", 123, 1).await.unwrap();
    assert_eq!(state(&store, &command).await, FirmwareJobState201::Pending);
    assert_eq!(database.release_jobs().len(), 1);
    assert_eq!(database.release_jobs()[0].1, "firmware");
    assert!(database.release_jobs()[0].0.starts_with("firmware201/"));
    dispatched(&store, &command, 2).await;
    reply(
        &store,
        &command,
        status(UpdateFirmwareStatus201::Accepted),
        3,
    )
    .await;
    assert_eq!(state(&store, &command).await, FirmwareJobState201::Accepted);
    for (offset, native) in [
        Downloading,
        Downloaded,
        SignatureVerified,
        Installing,
        InstallRebooting,
    ]
    .into_iter()
    .enumerate()
    {
        notify(
            &store,
            native,
            Some(123),
            10 + i64::try_from(offset).unwrap(),
        )
        .await;
        assert_eq!(database.release_jobs().len(), 1, "{native:?}");
    }
    assert_eq!(
        state(&store, &command).await,
        FirmwareJobState201::InstallRebooting
    );
    notify(&store, Installed, Some(123), 20).await;
    let evidence = evidence(&store, &command).await;
    assert_eq!(evidence.job.state, FirmwareJobState201::Installed);
    assert_eq!(evidence.job.notifications, 6);
    assert_eq!(evidence.job.last_status_at, Some(at(20)));
    assert_eq!(evidence.request_id, 123);
    assert!(evidence.secure);
    assert_eq!(
        evidence.reply,
        Some(status(UpdateFirmwareStatus201::Accepted))
    );
    assert_eq!(evidence.artifact, Some(artifact(true)));
    assert!(database.release_jobs().is_empty());
    // A late duplicate never reopens or changes the settled job.
    notify(&store, Downloading, Some(123), 30).await;
    assert_eq!(
        state(&store, &command).await,
        FirmwareJobState201::Installed
    );
}

#[tokio::test]
async fn notifications_match_only_their_request_id_and_cancellation_is_native() {
    use FirmwareStatus201::{Downloading, InstallationFailed, SignatureVerified};
    let database = Database::new();
    let store = database.open();
    let first = admit(&store, "first", 7, 1).await.unwrap();
    // Single flight: a second request cannot be admitted while the first awaits its reply.
    assert_eq!(
        admit(&store, "overlap", 8, 2).await.unwrap_err().code(),
        StorageErrorCode::Conflict
    );
    assert!(
        store
            .command_by_request_id(RequestId::new("overlap").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    dispatched(&store, &first, 2).await;
    reply(&store, &first, status(UpdateFirmwareStatus201::Accepted), 3).await;
    notify(&store, Downloading, Some(8), 4).await;
    notify(&store, Downloading, None, 5).await;
    assert_eq!(state(&store, &first).await, FirmwareJobState201::Accepted);
    notify(&store, Downloading, Some(7), 6).await;
    assert_eq!(
        state(&store, &first).await,
        FirmwareJobState201::Downloading
    );
    assert_eq!(
        admit(&store, "reuse", 7, 7).await.unwrap_err().code(),
        StorageErrorCode::Conflict
    );
    let second = admit(&store, "second", 9, 8).await.unwrap();
    assert_eq!(database.release_jobs().len(), 2);
    dispatched(&store, &second, 9).await;
    reply(
        &store,
        &second,
        status(UpdateFirmwareStatus201::AcceptedCanceled),
        10,
    )
    .await;
    assert_eq!(state(&store, &first).await, FirmwareJobState201::Cancelled);
    assert_eq!(database.release_jobs().len(), 1);
    // L01.FR.24 allows a failure report for the cancelled update; it never revives it.
    notify(&store, InstallationFailed, Some(7), 11).await;
    let cancelled = evidence(&store, &first).await;
    assert_eq!(cancelled.job.state, FirmwareJobState201::Cancelled);
    assert_eq!(cancelled.job.notifications, 2);
    notify(&store, SignatureVerified, Some(9), 12).await;
    assert_eq!(
        state(&store, &second).await,
        FirmwareJobState201::SignatureVerified
    );
    // A plain `Accepted` for a newer request supersedes the unresolved older one.
    let third = admit(&store, "third", 10, 13).await.unwrap();
    dispatched(&store, &third, 14).await;
    reply(
        &store,
        &third,
        status(UpdateFirmwareStatus201::Accepted),
        15,
    )
    .await;
    assert_eq!(
        state(&store, &second).await,
        FirmwareJobState201::Superseded
    );
    assert_eq!(database.release_jobs().len(), 1);
}

#[tokio::test]
async fn refusals_and_unsent_requests_release_without_progress() {
    let database = Database::new();
    let store = database.open();
    let refusals = [
        status(UpdateFirmwareStatus201::Rejected),
        status(UpdateFirmwareStatus201::InvalidCertificate),
        FirmwareReply201::Status {
            status: UpdateFirmwareStatus201::RevokedCertificate,
            reason_code: Some("Revoked".to_owned()),
        },
        FirmwareReply201::CallError {
            code: FirmwareCallError201::NotSupported,
        },
    ];
    for (index, reply_value) in refusals.into_iter().enumerate() {
        let id = i32::try_from(index).unwrap();
        let now = i64::from(id) * 10 + 1;
        let command = admit(&store, &format!("refused-{id}"), id, now)
            .await
            .unwrap();
        dispatched(&store, &command, now + 1).await;
        reply(&store, &command, reply_value.clone(), now + 2).await;
        let evidence = evidence(&store, &command).await;
        assert_eq!(evidence.job.state, FirmwareJobState201::Rejected);
        assert_eq!(evidence.reply, Some(reply_value));
        assert!(database.release_jobs().is_empty());
    }
    let unsent = admit(&store, "unsent", 99, 110).await.unwrap();
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
    assert_eq!(state(&store, &unsent).await, FirmwareJobState201::NotSent);
    assert!(database.release_jobs().is_empty());
}

#[tokio::test]
async fn native_reply_artifact_and_security_mode_are_immutable() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "secure", 4, 1).await.unwrap();
    dispatched(&store, &command, 2).await;
    let mut wrong_id = reply_result(&command, status(UpdateFirmwareStatus201::Accepted), 3);
    wrong_id.firmware_201.as_mut().unwrap().request_id = 5;
    assert!(persist(&store, wrong_id).await.is_err());
    let mut downgraded = reply_result(&command, status(UpdateFirmwareStatus201::Accepted), 3);
    let evidence_slot = downgraded.firmware_201.as_mut().unwrap();
    evidence_slot.secure = false;
    evidence_slot.artifact = Some(artifact(false));
    assert!(persist(&store, downgraded).await.is_err());
    let mut lying = reply_result(&command, status(UpdateFirmwareStatus201::Rejected), 3);
    lying.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted: true,
        error: None,
    };
    assert!(persist(&store, lying).await.is_err());
    let invalid_reason = reply_result(
        &command,
        FirmwareReply201::Status {
            status: UpdateFirmwareStatus201::Accepted,
            reason_code: Some("x".repeat(21)),
        },
        3,
    );
    assert!(persist(&store, invalid_reason).await.is_err());
    reply(
        &store,
        &command,
        status(UpdateFirmwareStatus201::Accepted),
        4,
    )
    .await;
    let changed = reply_result(&command, status(UpdateFirmwareStatus201::Rejected), 5);
    assert!(persist(&store, changed).await.is_err());
    // A stale lower-rank writer keeps the settled reply and current job state.
    dispatched(&store, &command, 6).await;
    let settled = evidence(&store, &command).await;
    assert!(settled.accepted());
    assert_eq!(settled.artifact, Some(artifact(true)));
    assert_eq!(settled.job.state, FirmwareJobState201::Accepted);
}

#[tokio::test]
async fn regressions_are_counted_and_identity_free_idle_needs_an_answered_job() {
    use FirmwareStatus201::{Downloaded, Downloading, Idle, SignatureVerified};
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "secure", 1, 1).await.unwrap();
    dispatched(&store, &command, 2).await;
    // Before the station answered, an identity-free `Idle` cannot be about this update.
    notify(&store, Idle, None, 3).await;
    assert_eq!(state(&store, &command).await, FirmwareJobState201::Pending);
    reply(
        &store,
        &command,
        status(UpdateFirmwareStatus201::Accepted),
        4,
    )
    .await;
    notify(&store, Downloaded, Some(1), 5).await;
    notify(&store, Downloading, Some(1), 6).await;
    let regressed = evidence(&store, &command).await;
    assert_eq!(regressed.job.state, FirmwareJobState201::Downloaded);
    assert_eq!(regressed.job.rejected_transitions, 1);
    assert_eq!(regressed.job.notifications, 2);
    notify(&store, Idle, None, 7).await;
    assert_eq!(
        state(&store, &command).await,
        FirmwareJobState201::StationIdle
    );
    assert!(database.release_jobs().is_empty());

    // A non-secure (L02) update carries no signature, so a signature status contradicts it.
    let plain = admit_mode(&store, "plain", 2, false, 10).await.unwrap();
    dispatched(&store, &plain, 11).await;
    let mut plain_reply = reply_result(&plain, status(UpdateFirmwareStatus201::Accepted), 12);
    let evidence_slot = plain_reply.firmware_201.as_mut().unwrap();
    evidence_slot.secure = false;
    evidence_slot.artifact = Some(artifact(false));
    persist(&store, plain_reply).await.unwrap();
    notify(&store, Downloaded, Some(2), 13).await;
    notify(&store, SignatureVerified, Some(2), 14).await;
    let plain_evidence = evidence(&store, &plain).await;
    assert!(!plain_evidence.secure);
    assert_eq!(plain_evidence.job.state, FirmwareJobState201::Downloaded);
    assert_eq!(plain_evidence.job.rejected_transitions, 1);
}
