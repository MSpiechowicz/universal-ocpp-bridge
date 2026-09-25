use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the ordered admission denials and socket assertion together.
async fn deny_unprivileged_wrong_scope_unknown_reference_and_inline_value_without_wire_effect() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, authorization, _, _) = setup(&store, running.handle.clone()).await;
    let (snapshot, provider, coordinator) = configured(
        &store,
        &running,
        snapshot.clone(),
        authorization,
        vec![protected(&snapshot, "VendorPassword", "private-secret")],
        FlowDiagnostics::default(),
    );
    let denied = scoped(
        coordinator.clone(),
        &snapshot,
        vec![AccessPermission::Control],
    );
    let error = denied
        .submit(write(
            &snapshot,
            "unprivileged",
            "VendorPassword",
            REFERENCE,
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), CommandAdmissionErrorCode::Unauthorized);
    let id = RequestId::new("unprivileged").unwrap();
    assert!(
        store
            .command_by_request_id(id.clone())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .command_result_by_request_id(id)
            .await
            .unwrap()
            .is_none()
    );
    let allowed = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![AccessPermission::PrivilegedControl],
    ));
    let wrong_key = allowed
        .submit(write(&snapshot, "wrong-key", "OtherPassword", REFERENCE))
        .await
        .unwrap();
    assert!(matches!(
        wrong_key.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::PolicyRejected,
                ..
            }
        }
    ));
    provider.revoke(REFERENCE).unwrap();
    let revoked = allowed
        .submit(write(&snapshot, "revoked", "VendorPassword", REFERENCE))
        .await
        .unwrap();
    assert!(matches!(
        revoked.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::PolicyRejected,
                ..
            }
        }
    ));
    let mut inline = write(&snapshot, "inline", "VendorPassword", REFERENCE);
    if let CommandOperation::Ocpp(operation) = &mut inline.request.operation {
        operation.payload = json!({"key":"VendorPassword","value":"private-secret"});
    }
    let invalid = allowed.submit(inline).await.unwrap();
    assert!(matches!(
        invalid.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::InvalidParameters,
                ..
            }
        }
    ));
    assert!(
        store
            .command_by_request_id(RequestId::new("inline").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    let raw_read = allowed
        .submit(read(
            &snapshot,
            "malicious-read",
            json!({"value":"private-secret"}),
        ))
        .await
        .unwrap();
    assert!(matches!(
        raw_read.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::InvalidParameters,
                ..
            }
        }
    ));
    assert!(
        store
            .command_by_request_id(RequestId::new("malicious-read").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        timeout(Duration::from_millis(80), running.peer.receive())
            .await
            .is_err(),
        "denied commands must not reach the wire"
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
