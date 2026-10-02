use super::*;

#[tokio::test]
async fn malformed_replies_errors_timeout_and_disconnect_remain_one_shot_after_reopen() {
    for mode in [
        "malformed",
        "wrong-action-status",
        "extra-field",
        "timeout",
        "disconnect",
        "callerror",
    ] {
        Box::pin(assert_failed_clear_is_one_shot_after_reopen(mode)).await;
    }
}

async fn assert_failed_clear_is_one_shot_after_reopen(mode: &str) {
    let mut running = session("ocpp1.6", Duration::from_millis(120)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let body = request(
        &state,
        mode,
        "ClearChargingProfile",
        json!({"id":117}),
        true,
    );
    let pending = {
        let coordinator = coordinator.clone();
        let body = body.clone();
        tokio::spawn(async move { coordinator.submit(body).await.unwrap() })
    };
    let call = receive_json(&mut running.peer).await;
    assert_eq!(call[2], "ClearChargingProfile");
    respond_to_failed_clear(&mut running, &call, mode).await;
    let result = pending.await.unwrap();
    assert_failed_clear_result(&result, mode);
    if mode == "timeout" {
        running
            .peer
            .send_text(json!([3,call[1],{"status":"Accepted"}]).to_string())
            .await
            .unwrap();
        timeout(Duration::from_secs(1), running.outputs.diagnostics.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .command_result_by_request_id(result.return_route.request_id.clone())
                .await
                .unwrap(),
            Some(result.clone())
        );
    }
    if mode == "disconnect" {
        running.task.wait().await.unwrap();
        running.server.abort();
        store.shutdown(Duration::from_secs(1)).await.unwrap();
    } else {
        finish(running, store).await;
    }
    drop(coordinator);
    let store = database.open();
    let mut next = session("ocpp1.6", Duration::from_secs(1)).await;
    let (_, _, coordinator) = configured(&store, &next).await;
    coordinator
        .recover_unresolved(PageLimit::new(100).unwrap())
        .await
        .unwrap();
    assert_eq!(coordinator.submit(body.clone()).await.unwrap(), result);
    no_call(&mut next).await;
    let mut conflict = body;
    let CommandOperation::Ocpp(operation) = &mut conflict.request.operation else {
        unreachable!();
    };
    operation.payload["id"] = json!(118);
    assert_eq!(
        coordinator.submit(conflict).await.unwrap_err().code(),
        CommandAdmissionErrorCode::InvalidRequest
    );
    no_call(&mut next).await;
    finish(next, store).await;
}

async fn respond_to_failed_clear(running: &mut RunningSession, call: &Value, mode: &str) {
    match mode {
        "disconnect" => running.peer.disconnect().await.unwrap(),
        "callerror" => running
            .peer
            .send_text(
                json!([4, call[1], "NotSupported", "private charger diagnostic", {}]).to_string(),
            )
            .await
            .unwrap(),
        "malformed" => running
            .peer
            .send_text(json!([3, call[1], {}]).to_string())
            .await
            .unwrap(),
        "wrong-action-status" => running
            .peer
            .send_text(json!([3,call[1],{"status":"Rejected"}]).to_string())
            .await
            .unwrap(),
        "extra-field" => running
            .peer
            .send_text(json!([3,call[1],{"status":"Accepted","clearedIds":[117]}]).to_string())
            .await
            .unwrap(),
        _ => {}
    }
}

fn assert_failed_clear_result(result: &CommandResult, mode: &str) {
    assert!(result.charging_profile_16.is_none());
    if mode == "callerror" {
        assert!(matches!(
            result.lifecycle,
            CommandLifecycle::ProtocolResponse {
                accepted: false,
                error: Some(CommandError {
                    code: CommandErrorCode::ProtocolRejected,
                    ..
                })
            }
        ));
        assert!(
            !serde_json::to_string(result)
                .unwrap()
                .contains("private charger diagnostic")
        );
    } else {
        assert!(matches!(
            result.lifecycle,
            CommandLifecycle::TransmissionUncertain { .. }
        ));
    }
}

#[tokio::test]
async fn captured_request_is_immutable_during_snapshot_updates_and_duplicate_admission() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut state, port, coordinator) = configured(&store, &running).await;
    let body = request(
        &state,
        "delayed-profile",
        "SetChargingProfile",
        set_payload(1, "0", "W"),
        false,
    );
    let pending = {
        let coordinator = coordinator.clone();
        let body = body.clone();
        tokio::spawn(async move { coordinator.submit(body).await.unwrap() })
    };
    let call = receive_json(&mut running.peer).await;
    state.resources[0].capabilities.operations.clear();
    port.update_committed(state).unwrap();
    assert_eq!(
        coordinator.submit(body.clone()).await.unwrap().lifecycle,
        CommandLifecycle::Dispatched
    );
    no_call(&mut running).await;
    running
        .peer
        .send_text(json!([2, "heartbeat", "Heartbeat", {}]).to_string())
        .await
        .unwrap();
    let incoming = timeout(Duration::from_secs(1), running.outputs.incoming.receive())
        .await
        .unwrap()
        .unwrap();
    incoming
        .responder
        .respond(&json!({"currentTime":"2026-09-01T02:00:00Z"}))
        .unwrap();
    assert_eq!(receive_json(&mut running.peer).await[1], "heartbeat");
    assert!(!pending.is_finished());
    running
        .peer
        .send_text(json!([3,call[1],{"status":"Accepted"}]).to_string())
        .await
        .unwrap();
    let result = pending.await.unwrap();
    accepted(&result);
    let ChargingProfileResult16::SetChargingProfile { request, .. } =
        result.charging_profile_16.as_ref().unwrap()
    else {
        panic!("set evidence");
    };
    assert_eq!(request.connector_id, 1);
    assert_eq!(
        request
            .cs_charging_profiles
            .charging_schedule
            .charging_rate_unit,
        CompositeScheduleRateUnit16::W
    );
    assert_eq!(
        request
            .cs_charging_profiles
            .charging_schedule
            .charging_schedule_period[0]
            .limit,
        ExactDecimal::new(0, 0)
    );
    assert_eq!(coordinator.submit(body).await.unwrap(), result);
    no_call(&mut running).await;
    finish(running, store).await;
}

#[tokio::test]
async fn fully_encoded_profile_call_has_a_hard_bound_before_enqueue() {
    let limits = RuntimeResourceLimits {
        maximum_ocpp_message_bytes: 512 * 1024,
        ..RuntimeResourceLimits::default()
    };
    let mut running = session_with_limits("ocpp1.6", Duration::from_secs(2), Some(limits)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;

    // This legal numeric exponent is still exactly 10. Its original wire lexeme
    // exceeds the profile-only cap while remaining below the shared runtime cap.
    let rate = format!("1e{}1", "0".repeat(256 * 1024));
    let external = request(
        &state,
        "encoded-profile-bound",
        "SetChargingProfile",
        set_payload(1, &rate, "A"),
        false,
    );
    let CommandOperation::Ocpp(operation) = &external.request.operation else {
        unreachable!();
    };
    assert_eq!(
        uob_protocol_adapter::command_registry::validate_privileged_operation(
            &external.request.resource,
            operation,
        ),
        Ok(()),
    );

    rejected(&coordinator.submit(external).await.unwrap());
    no_call(&mut running).await;
    finish(running, store).await;
}
