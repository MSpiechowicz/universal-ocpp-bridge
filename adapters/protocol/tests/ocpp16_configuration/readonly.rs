use super::*;

#[tokio::test]
async fn readonly_fact_cannot_be_cleared_by_a_later_read_on_the_same_socket() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, authorization, _, _) = setup(&store, running.handle.clone()).await;
    let (snapshot, _, coordinator) = configured(
        &store,
        &running,
        snapshot.clone(),
        authorization,
        vec![protected(&snapshot, "VendorProtected", "protected")],
        FlowDiagnostics::default(),
    );
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![AccessPermission::PrivilegedControl],
    ));
    for (id, readonly) in [("first-read", true), ("second-read", false)] {
        exchange(
            &mut running.peer,
            &commands,
            read(&snapshot, id, json!({"key":["VendorProtected"]})),
            json!({"key":["VendorProtected"]}),
            json!({"configurationKey":[{"key":"VendorProtected","readonly":readonly}]}),
        )
        .await;
    }
    let denied = commands
        .submit(write(
            &snapshot,
            "sticky-readonly",
            "VendorProtected",
            REFERENCE,
        ))
        .await
        .unwrap();
    assert!(matches!(
        denied.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::PolicyRejected,
                ..
            }
        }
    ));
    assert!(
        timeout(Duration::from_millis(80), running.peer.receive())
            .await
            .is_err(),
        "a denied ChangeConfiguration must not reach the wire"
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn readonly_facts_cover_large_replies_and_fail_closed_when_session_capacity_is_full() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, authorization, _, _) = setup(&store, running.handle.clone()).await;
    let overflow_reference = format!("cfg:{}", "3".repeat(64));
    let mut overflow_value = protected(&snapshot, "Key256", "protected");
    overflow_value.reference = overflow_reference.clone();
    let (snapshot, _, coordinator) = configured(
        &store,
        &running,
        snapshot.clone(),
        authorization,
        vec![protected(&snapshot, "Key128", "protected"), overflow_value],
        FlowDiagnostics::default(),
    );
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![AccessPermission::PrivilegedControl],
    ));
    let keys = (0..=128)
        .map(|index| json!({"key":format!("Key{index}"),"readonly":index == 128}))
        .collect::<Vec<_>>();
    exchange(
        &mut running.peer,
        &commands,
        read(&snapshot, "large-read", json!({})),
        json!({}),
        json!({"configurationKey":keys}),
    )
    .await;
    let denied = commands
        .submit(write(&snapshot, "large-readonly", "Key128", REFERENCE))
        .await
        .unwrap();
    assert!(matches!(
        denied.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::PolicyRejected,
                ..
            }
        }
    ));

    let rest = (129..=255)
        .map(|index| json!({"key":format!("Key{index}"),"readonly":false}))
        .collect::<Vec<_>>();
    exchange(
        &mut running.peer,
        &commands,
        read(&snapshot, "fill-facts", json!({})),
        json!({}),
        json!({"configurationKey":rest}),
    )
    .await;
    exchange(
        &mut running.peer,
        &commands,
        read(&snapshot, "overflow-facts", json!({})),
        json!({}),
        json!({"configurationKey":[{"key":"Key256","readonly":true}]}),
    )
    .await;
    let denied = commands
        .submit(write(
            &snapshot,
            "overflow-readonly",
            "Key256",
            &overflow_reference,
        ))
        .await
        .unwrap();
    assert!(matches!(
        denied.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::PolicyRejected,
                ..
            }
        }
    ));
    // The next authenticated read must be the next wire call, not either denied write.
    exchange(
        &mut running.peer,
        &commands,
        read(&snapshot, "after-denials", json!({})),
        json!({}),
        json!({"configurationKey":[]}),
    )
    .await;
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
