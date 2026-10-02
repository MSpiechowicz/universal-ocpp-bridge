use super::*;
fn query(state: &StationSnapshot, id: &str) -> ExternalCommand<Value> {
    request(
        state,
        id,
        "GetVariables",
        json!({"getVariableData":[{
        "component":{"name":"SmartChargingCtrlr","evse":{"id":1}},
        "variable":{"name":"ACPhaseSwitchingSupported"},"attributeType":"Actual"}]}),
        false,
    )
}
fn reply(status: &str, value: Option<&str>) -> Value {
    let mut entry = json!({"component":{"name":"SmartChargingCtrlr","evse":{"id":1}},
        "variable":{"name":"ACPhaseSwitchingSupported"},"attributeType":"Actual","attributeStatus":status});
    if let Some(value) = value {
        entry["attributeValue"] = json!(value);
    }
    json!({"getVariableResult":[entry]})
}
fn phase_profile() -> Value {
    let mut value = profile("0", "A");
    value["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]["phaseToUse"] =
        json!(3);
    value
}

#[tokio::test]
async fn strict_same_generation_actual_proof_is_required_and_later_failures_revoke_it() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    baseline(&mut running, &state, &coordinator).await;
    rejected(
        &coordinator
            .submit(request(
                &state,
                "unknown-phase",
                "SetChargingProfile",
                phase_profile(),
                false,
            ))
            .await
            .unwrap(),
    );
    no_call(&mut running).await;
    for (index, (status, value)) in [
        ("Accepted", Some("true")),
        ("Accepted", Some("false")),
        ("UnknownVariable", None),
        ("Accepted", Some("TRUE")),
        ("Accepted", None),
    ]
    .into_iter()
    .enumerate()
    {
        exchange(
            &mut running,
            &coordinator,
            query(&state, &format!("phase-query-{index}")),
            reply(status, value),
        )
        .await;
        let set = request(
            &state,
            &format!("phase-set-{index}"),
            "SetChargingProfile",
            phase_profile(),
            false,
        );
        if index == 0 {
            let result = exchange(
                &mut running,
                &coordinator,
                set,
                json!({"status":"Accepted"}),
            )
            .await;
            assert!(result.charging_profile_201.as_ref().unwrap().accepted());
        } else {
            rejected(&coordinator.submit(set).await.unwrap());
            no_call(&mut running).await;
        }
    }
    exchange(
        &mut running,
        &coordinator,
        query(&state, "positive-again"),
        reply("Accepted", Some("true")),
    )
    .await;
    let command = query(&state, "callerror-phase");
    let worker = coordinator.clone();
    let query_task = tokio::spawn(async move { worker.submit(command).await.unwrap() });
    let call = receive_json(&mut running.peer).await;
    running
        .peer
        .send_text(json!([4, call[1], "InternalError", "opaque", {}]).to_string())
        .await
        .unwrap();
    query_task.await.unwrap();
    rejected(
        &coordinator
            .submit(request(
                &state,
                "after-callerror",
                "SetChargingProfile",
                phase_profile(),
                false,
            ))
            .await
            .unwrap(),
    );
    no_call(&mut running).await;
    finish(running, store).await;
}

#[tokio::test]
async fn out_of_order_replies_cannot_restore_proof_and_reconnect_does_not_trust_persisted_true() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, port, coordinator) = configured(&store, &running).await;
    baseline(&mut running, &state, &coordinator).await;
    let old = query(&state, "old-phase");
    let worker = coordinator.clone();
    let old_task = tokio::spawn(async move { worker.submit(old).await.unwrap() });
    let old_call = receive_json(&mut running.peer).await;
    let new = query(&state, "new-phase");
    let worker = coordinator.clone();
    let new_task = tokio::spawn(async move { worker.submit(new).await.unwrap() });
    let new_call = receive_json(&mut running.peer).await;
    running
        .peer
        .send_text(json!([3, new_call[1], reply("Accepted", Some("false"))]).to_string())
        .await
        .unwrap();
    new_task.await.unwrap();
    running
        .peer
        .send_text(json!([3, old_call[1], reply("Accepted", Some("true"))]).to_string())
        .await
        .unwrap();
    old_task.await.unwrap();
    rejected(
        &coordinator
            .submit(request(
                &state,
                "out-of-order-denied",
                "SetChargingProfile",
                phase_profile(),
                false,
            ))
            .await
            .unwrap(),
    );
    no_call(&mut running).await;
    exchange(
        &mut running,
        &coordinator,
        query(&state, "persisted-positive"),
        reply("Accepted", Some("true")),
    )
    .await;
    port.detach_device_model();
    let (_, _, new_port, new_coordinator) = setup_device(&store, running.handle.clone()).await;
    new_port.update_committed(state.clone()).unwrap();
    rejected(
        &new_coordinator
            .submit(request(
                &state,
                "new-generation-denied",
                "SetChargingProfile",
                phase_profile(),
                false,
            ))
            .await
            .unwrap(),
    );
    no_call(&mut running).await;
    finish(running, store).await;
}
