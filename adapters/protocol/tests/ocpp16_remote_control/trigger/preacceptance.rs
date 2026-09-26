use super::*;

#[tokio::test]
async fn preacceptance_boot_and_dispatch_scope_snapshot_survive_later_topology_change() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    enable(&mut snapshot, &port);
    let commands = controls(coordinator, &snapshot);
    let mut pending = snapshot.clone();
    let boot = expected_frame("boot-notification", "new-station-boot");
    running.peer.send_text(boot.to_string()).await.unwrap();
    let incoming = running.outputs.incoming.receive().await.unwrap();
    let confirmation = v16::registration_call(
        boot.to_string().as_bytes(),
        &store,
        &mut pending,
        registration::RegistrationDecision::Pending,
        60,
        Clock.now(),
    )
    .await
    .unwrap();
    incoming.responder.respond(&confirmation[2]).unwrap();
    assert_eq!(receive_json(&mut running.peer).await, confirmation);
    port.update_committed(pending.clone()).unwrap();
    let boot_request = request(
        &pending,
        "trigger-boot",
        json!({"requestedMessage":"BootNotification"}),
        true,
    );
    let sender = commands.clone();
    let submission = tokio::spawn(async move { sender.submit(boot_request).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await,
        fixture("trigger-boot")
    );
    running
        .peer
        .send_text(expected_frame("trigger-accepted", "trigger-boot").to_string())
        .await
        .unwrap();
    let result = submission.await.unwrap();
    assert_eq!(
        result.trigger_observation.unwrap().expected_targets,
        vec![0]
    );

    let boot = expected_frame("boot-notification", "accepted-station-boot");
    running.peer.send_text(boot.to_string()).await.unwrap();
    let incoming = running.outputs.incoming.receive().await.unwrap();
    let confirmation = v16::registration_call(
        boot.to_string().as_bytes(),
        &store,
        &mut pending,
        registration::RegistrationDecision::Accepted,
        60,
        Clock.now(),
    )
    .await
    .unwrap();
    incoming.responder.respond(&confirmation[2]).unwrap();
    assert_eq!(receive_json(&mut running.peer).await, confirmation);
    snapshot = pending;
    port.update_committed(snapshot.clone()).unwrap();
    let all = request(
        &snapshot,
        "trigger-status-all",
        json!({"requestedMessage":"StatusNotification"}),
        true,
    );
    let sender = commands.clone();
    let submission = tokio::spawn(async move { sender.submit(all).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await,
        fixture("trigger-status-all")
    );
    let before = store
        .command_result_by_request_id(RequestId::new("trigger-status-all").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        before.trigger_observation.unwrap().expected_targets,
        vec![0, 1, 2]
    );
    snapshot.resources.pop();
    port.update_committed(snapshot).unwrap();
    running
        .peer
        .send_text(expected_frame("trigger-accepted", "trigger-status-all").to_string())
        .await
        .unwrap();
    let after = submission.await.unwrap();
    assert_eq!(
        after.trigger_observation.unwrap().expected_targets,
        vec![0, 1, 2]
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
