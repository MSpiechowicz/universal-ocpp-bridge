use super::*;

#[tokio::test]
async fn invalid_native_semantics_extensions_and_scopes_never_enqueue_a_call() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    baseline(&mut running, &state, &coordinator).await;
    let valid = profile("8.1", "A");
    for (index, payload) in invalid_set_payloads(&valid).into_iter().enumerate() {
        rejected(
            &coordinator
                .submit(request(
                    &state,
                    &format!("invalid-{index}"),
                    "SetChargingProfile",
                    payload,
                    false,
                ))
                .await
                .unwrap(),
        );
    }
    for (index,payload) in [json!({}),json!({"chargingProfileCriteria":{}}),
        json!({"chargingProfileId":1,"chargingProfileCriteria":{"evseId":1}}),
        json!({"chargingProfileCriteria":{"chargingProfilePurpose":"ChargingStationExternalConstraints"}}),
        json!({"id":1}),json!({"chargingProfileCriteria":{"stackLevel":-1}})].into_iter().enumerate()
    {
        rejected(&coordinator.submit(request(&state,&format!("clear-invalid-{index}"),"ClearChargingProfile",payload,true)).await.unwrap());
    }
    let mut connector = request(
        &state,
        "connector-scope",
        "SetChargingProfile",
        valid.clone(),
        false,
    );
    connector.request.resource = state.resources[1].resource.clone();
    rejected(&coordinator.submit(connector).await.unwrap());
    let id_clear = request(
        &state,
        "id-evse-scope",
        "ClearChargingProfile",
        json!({"chargingProfileId":1}),
        false,
    );
    rejected(&coordinator.submit(id_clear).await.unwrap());
    no_call(&mut running).await;
    let denied = scoped(coordinator.clone(), &state, vec![AccessPermission::Control]);
    assert!(
        denied
            .submit(request(
                &state,
                "ordinary-grant",
                "SetChargingProfile",
                valid,
                false
            ))
            .await
            .is_err()
    );
    no_call(&mut running).await;
    finish(running, store).await;
}
fn invalid_set_payloads(valid: &Value) -> Vec<Value> {
    let mut invalid = Vec::new();
    for (pointer, value) in [
        ("/chargingProfile/stackLevel", json!(-1)),
        (
            "/chargingProfile/chargingProfilePurpose",
            json!("ChargingStationExternalConstraints"),
        ),
        (
            "/chargingProfile/chargingProfilePurpose",
            json!("ChargingStationMaxProfile"),
        ),
        ("/chargingProfile/chargingProfileKind", json!("Relative")),
        ("/chargingProfile/validTo", json!("2026-09-01T01:00:00Z")),
        ("/chargingProfile/chargingSchedule/0/duration", json!(-1)),
        (
            "/chargingProfile/chargingSchedule/0/chargingSchedulePeriod/0/startPeriod",
            json!(1),
        ),
        (
            "/chargingProfile/chargingSchedule/0/chargingSchedulePeriod/0/limit",
            json!(8.11),
        ),
        (
            "/chargingProfile/chargingSchedule/0/chargingSchedulePeriod/0/numberPhases",
            json!(4),
        ),
        ("/evseId", json!(2)),
        ("/chargingProfile/id", json!(i64::from(i32::MAX) + 1)),
    ] {
        let mut candidate = valid.clone();
        *candidate.pointer_mut(pointer).unwrap() = value;
        invalid.push(candidate);
    }
    let mut unrequested_transaction = valid.clone();
    unrequested_transaction["chargingProfile"]["transactionId"] = json!("unrequested-tx");
    invalid.push(unrequested_transaction);
    let mut missing_recurrency = valid.clone();
    missing_recurrency["chargingProfile"]
        .as_object_mut()
        .unwrap()
        .remove("recurrencyKind");
    invalid.push(missing_recurrency);
    let mut missing_anchor = valid.clone();
    missing_anchor["chargingProfile"]["chargingSchedule"][0]
        .as_object_mut()
        .unwrap()
        .remove("startSchedule");
    invalid.push(missing_anchor);
    let mut unordered = valid.clone();
    unordered["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][1]["startPeriod"] =
        json!(0);
    invalid.push(unordered);
    for name in ["customData", "unknownField"] {
        let mut candidate = valid.clone();
        candidate["chargingProfile"][name] = json!({"vendorId":"vendor"});
        invalid.push(candidate);
    }
    let mut tariff = valid.clone();
    tariff["chargingProfile"]["chargingSchedule"][0]["salesTariff"] = json!({"id":1});
    invalid.push(tariff);
    let mut multiple = valid.clone();
    let schedule = multiple["chargingProfile"]["chargingSchedule"][0].clone();
    multiple["chargingProfile"]["chargingSchedule"]
        .as_array_mut()
        .unwrap()
        .push(schedule);
    invalid.push(multiple);
    for periods in [0, 1025] {
        let mut candidate = valid.clone();
        candidate["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"] =
            Value::Array(
                (0..periods)
                    .map(|start| json!({"startPeriod":start,"limit":0}))
                    .collect(),
            );
        invalid.push(candidate);
    }
    invalid
}

#[tokio::test]
async fn full_mode_requires_baseline_and_native_callerror_or_malformed_reply_never_fabricates_status()
 {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    rejected(
        &coordinator
            .submit(request(
                &state,
                "no-baseline",
                "SetChargingProfile",
                profile("0", "W"),
                false,
            ))
            .await
            .unwrap(),
    );
    no_call(&mut running).await;
    baseline(&mut running, &state, &coordinator).await;
    for (index, reply) in [
        json!({"status":"NotSupported"}),
        json!({"status":"Accepted","unknown":true}),
    ]
    .into_iter()
    .enumerate()
    {
        let result = exchange(
            &mut running,
            &coordinator,
            request(
                &state,
                &format!("malformed-{index}"),
                "SetChargingProfile",
                profile("0", "W"),
                false,
            ),
            reply,
        )
        .await;
        assert!(matches!(
            result.lifecycle,
            CommandLifecycle::TransmissionUncertain { .. }
        ));
        assert_eq!(result.charging_profile_201, None);
    }
    let command = request(
        &state,
        "callerror",
        "ClearChargingProfile",
        json!({"chargingProfileId":i32::MIN}),
        true,
    );
    let worker = coordinator.clone();
    let result = tokio::spawn(async move { worker.submit(command).await.unwrap() });
    let call = receive_json(&mut running.peer).await;
    running
        .peer
        .send_text(json!([4, call[1], "NotSupported", "private-opaque", {}]).to_string())
        .await
        .unwrap();
    let result = result.await.unwrap();
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::ProtocolResponse {
            accepted: false,
            ..
        }
    ));
    assert_eq!(result.charging_profile_201, None);
    finish(running, store).await;
}

#[tokio::test]
async fn transaction_commit_publication_window_blocks_profile_wire_until_committed_state_is_published()
 {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, port, coordinator) = configured(&store, &running).await;
    baseline(&mut running, &state, &coordinator).await;
    port.begin_snapshot_commit().unwrap();
    let blocked = coordinator
        .submit(request(
            &state,
            "commit-in-progress",
            "SetChargingProfile",
            profile("0", "A"),
            false,
        ))
        .await
        .unwrap();
    assert!(matches!(
        blocked.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::PolicyRejected,
                ..
            }
        }
    ));
    assert_eq!(blocked.charging_profile_201, None);
    no_call(&mut running).await;
    port.update_committed(state.clone()).unwrap();
    let accepted = exchange(
        &mut running,
        &coordinator,
        request(
            &state,
            "after-commit",
            "SetChargingProfile",
            profile("0", "A"),
            false,
        ),
        json!({"status":"Accepted"}),
    )
    .await;
    assert!(matches!(
        accepted.charging_profile_201,
        Some(ChargingProfileResult201::SetChargingProfile {
            status: SetChargingProfileStatus201::Accepted,
            ..
        })
    ));
    finish(running, store).await;
}
