use super::*;

fn refusal(result: &CommandResult) -> Option<CommandErrorCode> {
    match &result.lifecycle {
        CommandLifecycle::Rejected { error } => Some(error.code),
        _ => None,
    }
}

#[tokio::test]
async fn missing_option_or_capability_refuses_before_anything_is_sent() {
    let mut running = session("ocpp1.6", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    let commands = controls(coordinator.clone(), &snapshot);
    // The station option is off: nothing advertises the action.
    let disabled = commands
        .submit(extended(
            &snapshot,
            "disabled",
            json!({"requestedMessage":"Heartbeat"}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(
        refusal(&disabled),
        Some(CommandErrorCode::UnsupportedOperation)
    );

    // The option is on, but the operations that enable the Whitepaper classes are not.
    enable(&mut snapshot, &port, &[]);
    for (id, class) in [
        ("no-get-log", "LogStatusNotification"),
        ("no-signed-firmware", "FirmwareStatusNotification"),
    ] {
        let result = commands
            .submit(extended(
                &snapshot,
                id,
                json!({"requestedMessage": class}),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(
            refusal(&result),
            Some(CommandErrorCode::UnsupportedOperation),
            "{id}"
        );
        assert!(result.trigger_observation.is_none(), "{id}");
    }
    assert!(quiet(&mut running).await);
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn every_negative_corpus_request_is_refused_with_its_code_and_never_sent() {
    let mut running = session("ocpp1.6", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    enable(&mut snapshot, &port, &["GetLog", "SignedUpdateFirmware"]);
    let commands = controls(coordinator.clone(), &snapshot);
    let cases = fixture("security-negative-cases");
    let mut refused = 0;
    for case in cases["cases"].as_array().unwrap() {
        if case["wire"][0] != 2 || case["wire"][2] != "ExtendedTriggerMessage" {
            continue;
        }
        let expected = match case["bridge_refusal"].as_str() {
            Some("UnsupportedOperation") => CommandErrorCode::UnsupportedOperation,
            Some("InvalidParameters") | None => CommandErrorCode::InvalidParameters,
            Some(other) => panic!("unmapped refusal {other}"),
        };
        for station in [true, false] {
            let id = format!("{}-{station}", case["id"].as_str().unwrap());
            let external = extended(&snapshot, &id, case["wire"][3].clone(), station);
            let result = commands.submit(external).await.unwrap();
            assert_eq!(refusal(&result), Some(expected), "{id}");
            assert!(result.trigger_observation.is_none(), "{id}");
        }
        refused += 1;
    }
    assert_eq!(refused, 13);

    // Wrong schema identity, unrelated resource and scope mismatches.
    let mut wrong_schema = extended(
        &snapshot,
        "wrong-urn",
        json!({"requestedMessage":"Heartbeat"}),
        true,
    );
    if let CommandOperation::Ocpp(operation) = &mut wrong_schema.request.operation {
        operation.payload_schema =
            PayloadSchemaId::new("urn:OCPP:1.6:2019:12:TriggerMessageRequest").unwrap();
    }
    for (external, label) in [
        (wrong_schema, "wrong-urn"),
        (
            extended(
                &snapshot,
                "mismatch-station",
                json!({"requestedMessage":"MeterValues","connectorId":1}),
                true,
            ),
            "mismatch-station",
        ),
        (
            extended(
                &snapshot,
                "mismatch-connector",
                json!({"requestedMessage":"StatusNotification"}),
                false,
            ),
            "mismatch-connector",
        ),
        (
            extended(
                &snapshot,
                "spoofed-connector",
                json!({"requestedMessage":"MeterValues","connectorId":2}),
                false,
            ),
            "spoofed-connector",
        ),
    ] {
        let result = commands.submit(external).await.unwrap();
        assert_eq!(
            refusal(&result),
            Some(CommandErrorCode::InvalidParameters),
            "{label}"
        );
    }

    // Authorization is checked before the command is even admitted.
    let denied = scoped(coordinator, &snapshot, vec![AccessPermission::Control]);
    assert!(
        denied
            .submit(extended(
                &snapshot,
                "no-grant",
                json!({"requestedMessage":"Heartbeat"}),
                true
            ))
            .await
            .is_err()
    );
    assert!(quiet(&mut running).await);
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
