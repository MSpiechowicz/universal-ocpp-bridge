use super::support::*;

#[tokio::test]
async fn uncertain_dispatch_survives_restart_without_replay_and_native_facts_still_resolve() {
    let database = Database::new();
    let command = {
        let store = database.open();
        let command = admit(&store, "uncertain", LEGACY, 1).await.unwrap();
        dispatched(&store, &command, 2).await;
        persist(
            &store,
            result(
                &command,
                CommandLifecycle::TransmissionUncertain {
                    detail: "station disconnected".to_owned(),
                },
                3,
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            state(&store, &command).await,
            DiagnosticsJobState16::Uncertain
        );
        command
    };
    let store = database.open();
    store.recover_diagnostics_jobs_16(at(10)).await.unwrap();
    let recovered = jobs(&store).await;
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].state, DiagnosticsJobState16::Uncertain);
    assert_eq!(recovered[0].upload_id, Some(upload_id(&command)));
    assert_eq!(database.release_jobs().len(), 1);
    notify(&store, LogUploadStatus16::Uploading, false, None, None, 11).await;
    notify(
        &store,
        LogUploadStatus16::Uploaded,
        false,
        None,
        Some(stored(&command)),
        12,
    )
    .await;
    assert_eq!(
        state(&store, &command).await,
        DiagnosticsJobState16::Uploaded
    );
    assert!(database.release_jobs().is_empty());
}

#[tokio::test]
async fn startup_proves_never_dispatched_jobs_unsent_and_clears_single_flight() {
    let database = Database::new();
    let unsent = {
        let store = database.open();
        admit(&store, "unsent", LEGACY, 1).await.unwrap()
    };
    let store = database.open();
    store.recover_diagnostics_jobs_16(at(5)).await.unwrap();
    assert_eq!(state(&store, &unsent).await, DiagnosticsJobState16::NotSent);
    assert!(database.release_jobs().is_empty());
    let next = admit(&store, "next", security(3), 6).await.unwrap();
    // Bound but never answered: the request may have reached the station.
    store
        .bind_diagnostics_upload_16(next.request_id.clone(), upload_id(&next))
        .await
        .unwrap();
    drop(store);
    let store = database.open();
    store.recover_diagnostics_jobs_16(at(8)).await.unwrap();
    assert_eq!(state(&store, &next).await, DiagnosticsJobState16::Uncertain);
    assert_eq!(database.release_jobs().len(), 1);
}

#[tokio::test]
async fn startup_restores_a_missing_release_registration_and_deadlines_never_resolve() {
    let database = Database::new();
    let command = {
        let store = database.open();
        let command = admit(&store, "log", security(2), 1).await.unwrap();
        dispatched(&store, &command, 2).await;
        reply(&store, &command, log_status(GetLogStatus16::Accepted), 3).await;
        command
    };
    rusqlite::Connection::open(&database.0)
        .unwrap()
        .execute("DELETE FROM release_jobs", [])
        .unwrap();
    let store = database.open();
    store.recover_diagnostics_jobs_16(at(4)).await.unwrap();
    assert_eq!(database.release_jobs().len(), 1);
    store.expire_diagnostics_jobs_16(at(601)).await.unwrap();
    assert_eq!(
        state(&store, &command).await,
        DiagnosticsJobState16::TimedOut
    );
    assert_eq!(database.release_jobs().len(), 1);
    notify(
        &store,
        LogUploadStatus16::PermissionDenied,
        true,
        Some(2),
        None,
        700,
    )
    .await;
    assert_eq!(
        state(&store, &command).await,
        DiagnosticsJobState16::PermissionDenied
    );
    assert!(database.release_jobs().is_empty());
}

#[tokio::test]
async fn tampered_durable_job_fails_closed_on_read() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "tampered", LEGACY, 1).await.unwrap();
    dispatched(&store, &command, 2).await;
    reply(&store, &command, file("a.log"), 3).await;
    drop(store);
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    connection
        .execute(
            "UPDATE diagnostics16_jobs SET payload=json_set(payload,'$.state','uploading')",
            [],
        )
        .unwrap();
    drop(connection);
    let store = database.open();
    let error = store
        .command_result_by_request_id(command.request_id.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code(), StorageErrorCode::IntegrityFailure);
    // An uploaded state without provider facts is also refused.
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    connection
        .execute(
            "UPDATE diagnostics16_jobs SET payload=json_set(payload,'$.state','uploaded')",
            [],
        )
        .unwrap();
    drop(connection);
    assert!(store.diagnostics_jobs_16(station()).await.is_err());
}
