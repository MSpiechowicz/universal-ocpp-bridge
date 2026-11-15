use super::*;

#[tokio::test]
async fn a_delayed_reply_does_not_block_heartbeat_handling() {
    let mut running = session("ocpp1.6", Duration::from_secs(5)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    enable(&mut snapshot, &port, &["GetLog"]);
    let commands = controls(coordinator, &snapshot);
    let external = extended(
        &snapshot,
        "slow-log-status",
        json!({"requestedMessage":"LogStatusNotification"}),
        true,
    );
    let sender = commands.clone();
    let submission = tokio::spawn(async move { sender.submit(external).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await,
        with_id("extended-trigger-log-status", "slow-log-status")
    );

    // The station is still deciding; its own Heartbeat is answered meanwhile.
    running
        .peer
        .send_text(fixture("heartbeat").to_string())
        .await
        .unwrap();
    let incoming = running.outputs.incoming.receive().await.unwrap();
    incoming
        .responder
        .respond(&json!({"currentTime":"2026-09-01T02:00:00Z"}))
        .unwrap();
    let answer = receive_json(&mut running.peer).await;
    assert_eq!(answer[0], 3);
    assert_eq!(answer[1], "fixture-16-heartbeat-001");
    assert!(!submission.is_finished());

    running
        .peer
        .send_text(with_id("extended-trigger-accepted", "slow-log-status").to_string())
        .await
        .unwrap();
    let result = submission.await.unwrap();
    accepted(&result);
    assert_eq!(
        result.trigger_observation.unwrap().requested_class,
        TriggerMessageClass::LogStatusNotification
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn timeout_and_disconnect_stay_uncertain_and_are_never_replayed() {
    let mut running = session("ocpp1.6", Duration::from_millis(150)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    enable(&mut snapshot, &port, &["SignedUpdateFirmware"]);
    let commands = controls(coordinator, &snapshot);
    let slow = extended(
        &snapshot,
        "slow-firmware",
        json!({"requestedMessage":"FirmwareStatusNotification"}),
        true,
    );
    let sender = commands.clone();
    let sent = slow.clone();
    let submission = tokio::spawn(async move { sender.submit(sent).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await,
        with_id("extended-trigger-firmware-status", "slow-firmware")
    );
    let result = submission.await.unwrap();
    assert!(matches!(
        &result.lifecycle,
        CommandLifecycle::TransmissionUncertain { .. }
    ));
    let observation = result.trigger_observation.as_ref().unwrap();
    assert_eq!(observation.native_response, None);
    assert_eq!(
        observation.requested_class,
        TriggerMessageClass::SignedFirmwareStatusNotification
    );

    // A reply after the deadline changes nothing, and a retry replays the stored answer.
    running
        .peer
        .send_text(with_id("extended-trigger-accepted", "slow-firmware").to_string())
        .await
        .unwrap();
    assert_eq!(commands.submit(slow).await.unwrap(), result);
    assert!(quiet(&mut running).await);

    let disconnect = extended(
        &snapshot,
        "disconnect-meter",
        json!({"requestedMessage":"MeterValues","connectorId":1}),
        false,
    );
    let sender = commands.clone();
    let sent = disconnect.clone();
    let submission = tokio::spawn(async move { sender.submit(sent).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await,
        with_id("extended-trigger-meter-connector-1", "disconnect-meter")
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    let uncertain = submission.await.unwrap();
    assert!(!matches!(
        &uncertain.lifecycle,
        CommandLifecycle::ProtocolResponse { accepted: true, .. }
    ));
    assert_eq!(
        uncertain
            .trigger_observation
            .as_ref()
            .unwrap()
            .native_response,
        None
    );
    assert_eq!(commands.submit(disconnect).await.unwrap(), uncertain);

    // After a restart the stored results are unchanged and nothing is sent again.
    let reopened = database.open();
    for (id, expected) in [("slow-firmware", result), ("disconnect-meter", uncertain)] {
        assert_eq!(
            reopened
                .command_result_by_request_id(RequestId::new(id).unwrap())
                .await
                .unwrap(),
            Some(expected)
        );
    }
    running.server.abort();
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn a_station_callerror_is_surfaced_like_the_plain_trigger() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    enable(&mut snapshot, &port, &["TriggerMessage"]);
    let commands = controls(coordinator, &snapshot);
    let plain = {
        let mut external = command(
            &snapshot,
            "callerror-plain",
            protocol("TriggerMessage", json!({"requestedMessage":"Heartbeat"})),
        );
        external.request.resource = snapshot.station.clone();
        external
    };
    let mut outcomes = Vec::new();
    for external in [
        plain,
        extended(
            &snapshot,
            "callerror-extended",
            json!({"requestedMessage":"Heartbeat"}),
            true,
        ),
    ] {
        let id = external.request.request_id.as_str().to_owned();
        let sender = commands.clone();
        let submission = tokio::spawn(async move { sender.submit(external).await.unwrap() });
        assert_eq!(receive_json(&mut running.peer).await[1], id.as_str());
        running
            .peer
            .send_text(json!([4, id, "NotSupported", "private detail", {}]).to_string())
            .await
            .unwrap();
        let result = submission.await.unwrap();
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("private detail")
        );
        let observation = result.trigger_observation.unwrap();
        outcomes.push((
            result.lifecycle,
            observation.native_response,
            observation.status,
        ));
    }
    assert_eq!(outcomes[0], outcomes[1]);
    assert!(matches!(
        outcomes[1].0,
        CommandLifecycle::ProtocolResponse {
            accepted: false,
            ..
        }
    ));
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn boot_notification_is_allowed_only_before_acceptance() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    enable(&mut snapshot, &port, &[]);
    let commands = controls(coordinator, &snapshot);
    let boot = fixture("extended-trigger-boot");
    let id = boot[1].as_str().unwrap().to_owned();

    // Accepted station: the boot trigger is a policy refusal.
    let result = commands
        .submit(extended(&snapshot, &id, boot[3].clone(), true))
        .await
        .unwrap();
    assert!(matches!(&result.lifecycle,
        CommandLifecycle::Rejected { error } if error.code == CommandErrorCode::PolicyRejected));
    assert!(quiet(&mut running).await);

    let mut pending = snapshot.clone();
    for value in &mut pending.current_values {
        if value.point_id.as_str() == "ocpp16/registration/status" {
            value.value = Some(TypedValue::Text("Pending".to_owned()));
        }
    }
    port.update_committed(pending.clone()).unwrap();

    // Pending station: every other class is refused, the boot trigger is sent.
    for (name, class) in [
        ("pending-heartbeat", "Heartbeat"),
        ("pending-status", "StatusNotification"),
    ] {
        let result = commands
            .submit(extended(
                &pending,
                name,
                json!({"requestedMessage": class}),
                true,
            ))
            .await
            .unwrap();
        assert!(
            matches!(&result.lifecycle,
            CommandLifecycle::Rejected { error } if error.code == CommandErrorCode::PolicyRejected),
            "{name}"
        );
    }
    assert!(quiet(&mut running).await);
    let sender = commands.clone();
    let external = extended(&pending, "pending-boot", boot[3].clone(), true);
    let submission = tokio::spawn(async move { sender.submit(external).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await,
        with_id("extended-trigger-boot", "pending-boot")
    );
    running
        .peer
        .send_text(with_id("extended-trigger-accepted", "pending-boot").to_string())
        .await
        .unwrap();
    let result = submission.await.unwrap();
    accepted(&result);
    let observation = result.trigger_observation.unwrap();
    assert_eq!(
        observation.requested_class,
        TriggerMessageClass::BootNotification
    );
    assert_eq!(observation.expected_targets, vec![0]);
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
