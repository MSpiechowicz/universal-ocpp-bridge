use super::*;

fn transaction(resource: &ResourceRef, native: i32) -> TransactionSnapshot {
    serde_json::from_value(json!({
        "transaction_id":"ongoing", "resource":resource,"state":"active","started_at":"2026-09-01T01:00:00Z",
        "ocpp16":{"transaction_id":native,"start_message_id":"native-start","start_fingerprint":"fingerprint",
            "authorization_status":"Accepted","authorization_expiry":null,"identity_reference":null,"meter_start":0,
            "reservation_id":null,"stop_message_id":null,"stop_fingerprint":null,"stop_identity_fingerprint":null,
            "meter_stop":null,"stop_reason":null}
    })).unwrap()
}
fn tx_payload(native: i32) -> Value {
    let mut payload = set_payload(1, "0", "A");
    payload["csChargingProfiles"]["chargingProfilePurpose"] = json!("TxProfile");
    payload["csChargingProfiles"]["transactionId"] = json!(native);
    payload
}

#[tokio::test]
async fn native_tx_profile_requires_unique_established_ongoing_identity_at_dispatch() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut state, port, coordinator) = configured(&store, &running).await;
    let active = transaction(&state.resources[0].resource, i32::MIN);
    state.transactions.push(active.clone());
    assert_ongoing_transaction_profiles(&mut running, &mut state, &port, &coordinator).await;
    assert_invalid_transaction_profiles(&mut running, &mut state, &port, &coordinator, &active)
        .await;
    // Capture while active, then end before dispatch: live state, not admission intent, wins.
    state.transactions = vec![active];
    port.update_committed(state.clone()).unwrap();
    let admitted = request(
        &state,
        "ended-before-enqueue",
        "SetChargingProfile",
        tx_payload(i32::MIN),
        false,
    )
    .admit(Clock.now());
    state.transactions[0].ended_at = Some(Clock.now());
    port.update_committed(state.clone()).unwrap();
    assert!(matches!(
        port.dispatch(admitted).await.unwrap(),
        CommandDispatchOutcome::NotTransmitted {
            error: CommandError {
                code: CommandErrorCode::InvalidParameters,
                ..
            }
        }
    ));
    no_call(&mut running).await;
    finish(running, store).await;
}

async fn assert_ongoing_transaction_profiles(
    running: &mut RunningSession,
    state: &mut StationSnapshot,
    port: &RemoteControlSession,
    coordinator: &Arc<Coordinator>,
) {
    for (index, phase) in [
        TransactionState::Pending,
        TransactionState::Active,
        TransactionState::Suspended,
    ]
    .into_iter()
    .enumerate()
    {
        state.transactions[0].state = phase;
        port.update_committed(state.clone()).unwrap();
        let result = exchange(
            running,
            coordinator,
            request(
                state,
                &format!("ongoing-{index}"),
                "SetChargingProfile",
                tx_payload(i32::MIN),
                false,
            ),
            json!({"status":"Accepted"}),
        )
        .await;
        accepted(&result);
        let ChargingProfileResult16::SetChargingProfile { request, .. } =
            result.charging_profile_16.unwrap()
        else {
            panic!("set evidence");
        };
        assert_eq!(request.cs_charging_profiles.transaction_id, Some(i32::MIN));
    }
}

async fn assert_invalid_transaction_profiles(
    running: &mut RunningSession,
    state: &mut StationSnapshot,
    port: &RemoteControlSession,
    coordinator: &Arc<Coordinator>,
    active: &TransactionSnapshot,
) {
    for (index, mode) in [
        "ended",
        "uncertain",
        "missing-native",
        "mismatched",
        "ambiguous-child",
        "duplicate-native",
    ]
    .into_iter()
    .enumerate()
    {
        state.transactions = vec![active.clone()];
        match mode {
            "ended" => state.transactions[0].state = TransactionState::Ended,
            "uncertain" => state.transactions[0].state = TransactionState::Uncertain,
            "missing-native" => state.transactions[0].ocpp16 = None,
            "mismatched" => {
                state.transactions[0]
                    .ocpp16
                    .as_mut()
                    .unwrap()
                    .transaction_id = 117;
            }
            "ambiguous-child" | "duplicate-native" => {
                let mut other = active.clone();
                other.transaction_id = TransactionId::new("another").unwrap();
                if mode == "duplicate-native" {
                    other.resource = state.resources[1].resource.clone();
                }
                state.transactions.push(other);
            }
            _ => unreachable!(),
        }
        port.update_committed(state.clone()).unwrap();
        rejected(
            &coordinator
                .submit(request(
                    state,
                    &format!("tx-invalid-{index}"),
                    "SetChargingProfile",
                    tx_payload(i32::MIN),
                    false,
                ))
                .await
                .unwrap(),
        );
        no_call(running).await;
    }
}

#[tokio::test]
async fn canonical_limit_never_acquires_full_profile_evidence_or_zero_semantics() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut state, port, coordinator) = configured(&store, &running).await;
    state
        .transactions
        .push(transaction(&state.resources[0].resource, 117));
    state.resources[0]
        .capabilities
        .operations
        .push(SupportedOperation {
            operation: Operation::SetChargingLimit,
            parameters: vec![],
        });
    port.update_committed(state.clone()).unwrap();
    let canonical = command(
        &state,
        "canonical",
        CommandOperation::SetChargingLimit(ChargingLimit {
            value: ExactDecimal::new(75, 1),
            unit: EngineeringUnit::Ampere,
            phases: Some(1),
        }),
    );
    let submit = {
        let coordinator = coordinator.clone();
        tokio::spawn(async move { coordinator.submit(canonical).await.unwrap() })
    };
    let call = receive_json(&mut running.peer).await;
    assert_eq!(call[2], "SetChargingProfile");
    assert_eq!(
        call[3]["csChargingProfiles"]["chargingProfilePurpose"],
        "TxProfile"
    );
    assert_eq!(
        call[3]["csChargingProfiles"]["chargingProfileKind"],
        "Relative"
    );
    assert_eq!(call[3]["csChargingProfiles"]["stackLevel"], 0);
    running
        .peer
        .send_text(json!([3,call[1],{"status":"Accepted"}]).to_string())
        .await
        .unwrap();
    let result = submit.await.unwrap();
    accepted(&result);
    assert!(result.charging_profile_16.is_none());
    let zero = command(
        &state,
        "canonical-zero",
        CommandOperation::SetChargingLimit(ChargingLimit {
            value: ExactDecimal::new(0, 0),
            unit: EngineeringUnit::Ampere,
            phases: Some(1),
        }),
    );
    rejected(&coordinator.submit(zero).await.unwrap());
    no_call(&mut running).await;
    finish(running, store).await;
}
