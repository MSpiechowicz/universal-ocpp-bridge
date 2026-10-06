use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)] // One table per action keeps every no-wire denial visible.
async fn invalid_scope_criteria_and_authority_never_enqueue_a_call() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let composite = [
        (json!({"evseId":1,"duration":60}), true),
        (json!({"evseId":0,"duration":60}), false),
        (json!({"evseId":2,"duration":60}), false),
        (json!({"evseId":0,"duration":0}), true),
        (json!({"evseId":0,"duration":-1}), true),
        (json!({"evseId":0,"duration":60.5}), true),
        (json!({"evseId":0,"duration":2_147_483_648_i64}), true),
        (json!({"evseId":0}), true),
        (
            json!({"evseId":0,"duration":60,"chargingRateUnit":"kW"}),
            true,
        ),
        (
            json!({"evseId":0,"duration":60,"chargingRateUnit":null}),
            true,
        ),
        (json!({"evseId":0,"duration":60,"unknown":true}), true),
        (
            json!({"evseId":0,"duration":60,"customData":{"vendorId":"v"}}),
            true,
        ),
    ];
    for (index, (payload, station)) in composite.into_iter().enumerate() {
        let request = query(
            &state,
            &format!("k08-{index}"),
            "GetCompositeSchedule",
            payload,
            station,
        );
        rejected(&coordinator.submit(request).await.unwrap());
    }
    let criterion = |value: Value| json!({"requestId":1,"chargingProfile":value});
    let reports = [
        (criterion(json!({})), true),
        (
            criterion(json!({"chargingProfileId":[1],"stackLevel":0})),
            true,
        ),
        (criterion(json!({"chargingProfileId":[]})), true),
        (
            criterion(json!({"chargingProfileId":(0..65).collect::<Vec<_>>()})),
            true,
        ),
        (criterion(json!({"stackLevel":-1})), true),
        (
            criterion(json!({"chargingLimitSource":["CSO","CSO"]})),
            true,
        ),
        (criterion(json!({"chargingLimitSource":["Grid"]})), true),
        (criterion(json!({"chargingProfilePurpose":"Unknown"})), true),
        (
            criterion(json!({"stackLevel":0,"customData":{"vendorId":"v"}})),
            true,
        ),
        (
            json!({"requestId":1,"evseId":-1,"chargingProfile":{"stackLevel":0}}),
            true,
        ),
        (
            json!({"requestId":1,"chargingProfile":{"stackLevel":0}}),
            false,
        ),
        (
            json!({"requestId":1,"evseId":0,"chargingProfile":{"stackLevel":0}}),
            false,
        ),
        (
            json!({"requestId":1,"evseId":2,"chargingProfile":{"stackLevel":0}}),
            false,
        ),
        (
            json!({"requestId":2_147_483_648_i64,"chargingProfile":{"stackLevel":0}}),
            true,
        ),
        (json!({"chargingProfile":{"stackLevel":0}}), true),
    ];
    for (index, (payload, station)) in reports.into_iter().enumerate() {
        let request = query(
            &state,
            &format!("k09-{index}"),
            "GetChargingProfiles",
            payload,
            station,
        );
        rejected(&coordinator.submit(request).await.unwrap());
    }
    // Connector resources never address K08/K09.
    let mut connector = query(
        &state,
        "connector",
        "GetCompositeSchedule",
        json!({"evseId":1,"duration":60}),
        false,
    );
    connector.request.resource = state.resources[1].resource.clone();
    assert!(matches!(
        coordinator.submit(connector).await.unwrap().lifecycle,
        CommandLifecycle::Rejected { .. }
    ));
    let mut wrong_schema = query(
        &state,
        "schema",
        "GetCompositeSchedule",
        json!({"evseId":0,"duration":60}),
        true,
    );
    if let CommandOperation::Ocpp(operation) = &mut wrong_schema.request.operation {
        operation.payload_schema =
            PayloadSchemaId::new("urn:OCPP:1.6:2019:12:GetCompositeScheduleRequest").unwrap();
    }
    rejected(&coordinator.submit(wrong_schema).await.unwrap());
    no_call(&mut running).await;
    let denied = scoped(coordinator.clone(), &state, vec![AccessPermission::Control]);
    for (id, action, payload) in [
        (
            "ordinary-k08",
            "GetCompositeSchedule",
            json!({"evseId":0,"duration":60}),
        ),
        (
            "ordinary-k09",
            "GetChargingProfiles",
            criterion(json!({"stackLevel":0})),
        ),
    ] {
        assert!(
            denied
                .submit(query(&state, id, action, payload, true))
                .await
                .is_err()
        );
    }
    no_call(&mut running).await;
    finish(running, store).await;
}

#[tokio::test]
async fn reused_request_ids_and_unbound_reports_are_refused() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let payload = json!({"requestId":7,"chargingProfile":{"stackLevel":0}});
    let (_, first) = exchange(
        &mut running,
        &coordinator,
        query(
            &state,
            "first",
            "GetChargingProfiles",
            payload.clone(),
            true,
        ),
        json!({"status":"NoProfiles"}),
    )
    .await;
    assert!(first.charging_profiles_201.is_some());
    // A native request ID stays retired on this connection, separately from the CALL ID.
    let reused = coordinator
        .submit(query(
            &state,
            "reused",
            "GetChargingProfiles",
            payload,
            true,
        ))
        .await
        .unwrap();
    assert!(matches!(
        reused.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::PolicyRejected,
                ..
            }
        }
    ));
    no_call(&mut running).await;
    finish(running, store).await;

    // Without an advertised action the report namespace stays default-off.
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let store = database.open();
    let (state, _, _, coordinator) = setup_device(&store, running.handle.clone()).await;
    let unadvertised = coordinator
        .submit(query(
            &state,
            "off",
            "GetChargingProfiles",
            json!({"requestId":8,"chargingProfile":{"stackLevel":0}}),
            true,
        ))
        .await
        .unwrap();
    rejected(&unadvertised);
    let frame = report(
        &mut running,
        "unsolicited",
        &fragment(8, 0, "CSO", false, &[profile(1, "TxDefaultProfile", 0)]),
    )
    .await;
    assert_eq!(
        (frame[0].as_u64(), frame[2].as_str()),
        (Some(4), Some("NotImplemented"))
    );
    finish(running, store).await;
}
