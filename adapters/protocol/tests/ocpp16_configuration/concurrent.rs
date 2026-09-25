use super::*;

#[tokio::test]
async fn concurrent_explicit_reads_merge_on_separate_store_handles_and_survive_reopen() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, authorization, port, _) = setup(&store, running.handle.clone()).await;
    let (snapshot, _, coordinator) = configured(
        &store,
        &running,
        snapshot.clone(),
        authorization,
        vec![protected(&snapshot, "VendorPassword", "private-secret")],
        FlowDiagnostics::default(),
    );
    let commands = Arc::new(scoped(
        coordinator.clone(),
        &snapshot,
        vec![AccessPermission::PrivilegedControl],
    ));
    exchange(
        &mut running.peer,
        &commands,
        write(&snapshot, "concurrent-write", "VendorPassword", REFERENCE),
        json!({"key":"VendorPassword","value":"private-secret"}),
        json!({"status":"Accepted"}),
    )
    .await;
    for id in ["concurrent-read-a", "concurrent-read-b"] {
        exchange(
            &mut running.peer,
            &commands,
            read(&snapshot, id, json!({"key":["VendorPassword"]})),
            json!({"key":["VendorPassword"]}),
            json!({"configurationKey":[{"key":"VendorPassword","readonly":false,"value":"private-secret"}]}),
        )
        .await;
    }

    let other_store = database.open();
    let other_coordinator = Coordinator::new(Arc::new(other_store.clone()), port, Arc::new(Clock));
    let write_id = RequestId::new("concurrent-write").unwrap();
    let (a, b) = tokio::join!(
        coordinator.reconcile_configuration_observation(
            write_id.clone(),
            RequestId::new("concurrent-read-a").unwrap()
        ),
        other_coordinator.reconcile_configuration_observation(
            write_id.clone(),
            RequestId::new("concurrent-read-b").unwrap()
        )
    );
    assert!(a.unwrap().is_some());
    assert!(b.unwrap().is_some());
    let duplicate = other_coordinator
        .reconcile_configuration_observation(
            write_id.clone(),
            RequestId::new("concurrent-read-a").unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(duplicate.configuration_observations.len(), 2);
    let missing = other_store
        .append_configuration_observation(
            RequestId::new("nonexistent-write").unwrap(),
            duplicate.configuration_observations[0].clone(),
        )
        .await
        .unwrap();
    assert!(missing.is_none());

    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    other_store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    let durable = reopened
        .command_result_by_request_id(write_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(durable.configuration_observations.len(), 2);
    for id in ["concurrent-read-a", "concurrent-read-b"] {
        let entry = durable
            .configuration_observations
            .iter()
            .find(|entry| entry.read_request_id.as_str() == id)
            .expect("both read identities survive");
        assert_eq!(entry.key.key, "VendorPassword");
        assert!(entry.key.redacted && entry.key.value.is_none());
    }
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}
