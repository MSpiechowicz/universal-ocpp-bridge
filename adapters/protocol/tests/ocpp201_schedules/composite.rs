use super::*;

const GRID: &str = r#"{"status":"Accepted","customData":{"vendorId":"v","private":"opaque-vendor"},
    "schedule":{"evseId":0,"duration":86400,"scheduleStart":"2026-09-01T04:00:00+02:00",
    "chargingRateUnit":"W","chargingSchedulePeriod":[
        {"startPeriod":0,"limit":900719925474099.1},
        {"startPeriod":3600,"limit":0,"numberPhases":1,"phaseToUse":2},
        {"startPeriod":7200,"limit":8.10000e0,"numberPhases":3,"customData":{"vendorId":"v"}}]}}"#;

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep both scopes, exact values and the reopened evidence together.
async fn composite_schedules_retain_exact_scope_periods_and_reasons_after_reopen() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let (call, grid) = exchange(
        &mut running,
        &coordinator,
        query(
            &state,
            "grid",
            "GetCompositeSchedule",
            json!({"evseId":0,"duration":86400,"chargingRateUnit":"W"}),
            true,
        ),
        raw(GRID),
    )
    .await;
    assert_eq!(call[2], "GetCompositeSchedule");
    assert_eq!(
        call[3],
        json!({"evseId":0,"duration":86400,"chargingRateUnit":"W"})
    );
    accepted(&grid);
    assert_eq!(grid.schema_version, ContractVersion::V1_SCHEDULES_201);
    let evidence = grid.composite_schedule_201.as_ref().unwrap();
    assert_eq!(
        evidence.request,
        CompositeScheduleRequest201 {
            evse_id: 0,
            duration: 86400,
            charging_rate_unit: Some(ChargingScheduleRateUnit201::W),
        }
    );
    assert_eq!(evidence.status, CompositeScheduleStatus201::Accepted);
    assert_eq!(evidence.reason_code, None);
    let schedule = evidence.schedule.as_ref().unwrap();
    assert_eq!(schedule.schedule_start, time("2026-09-01T02:00:00Z"));
    let periods = &schedule.charging_schedule_period;
    assert_eq!(
        periods
            .iter()
            .map(|period| period.limit.to_string())
            .collect::<Vec<_>>(),
        ["900719925474099.1", "0", "8.1"]
    );
    assert_eq!(
        (periods[0].number_phases, periods[1].phase_to_use),
        (None, Some(2))
    );
    assert_eq!(periods[2].number_phases, Some(3));
    assert!(
        !serde_json::to_string(&grid)
            .unwrap()
            .contains("opaque-vendor")
    );
    assert!(grid.observed_effects.is_empty());

    let (call, rejected) = exchange(
        &mut running,
        &coordinator,
        query(
            &state,
            "evse-rejected",
            "GetCompositeSchedule",
            json!({"evseId":1,"duration":60}),
            false,
        ),
        json!({"status":"Rejected","statusInfo":{"reasonCode":"unknownEVSE",
            "additionalInfo":"private native detail","customData":{"vendorId":"v"}}}),
    )
    .await;
    assert_eq!(call[3], json!({"evseId":1,"duration":60}));
    assert!(matches!(
        &rejected.lifecycle,
        CommandLifecycle::ProtocolResponse {
            accepted: false,
            error: Some(CommandError { code: CommandErrorCode::ProtocolRejected, detail: Some(detail) })
        } if detail == "Rejected"
    ));
    let evidence = rejected.composite_schedule_201.as_ref().unwrap();
    assert_eq!(evidence.status, CompositeScheduleStatus201::Rejected);
    assert_eq!(
        evidence.reason_code,
        Some(SmartChargingReason201::UnknownEvse)
    );
    assert_eq!(evidence.schedule, None);
    assert!(
        !serde_json::to_string(&rejected)
            .unwrap()
            .contains("private native detail")
    );

    // A Rejected reply may carry a schedule; it is validated and retained, and an unlisted
    // reason code is omitted rather than guessed.
    let (_, unusual) = exchange(
        &mut running,
        &coordinator,
        query(
            &state,
            "evse-unusual",
            "GetCompositeSchedule",
            json!({"evseId":1,"duration":60}),
            false,
        ),
        json!({"status":"Rejected","statusInfo":{"reasonCode":"VendorSpecific"},"schedule":{
            "evseId":1,"duration":60,"scheduleStart":"2026-09-01T02:00:00Z","chargingRateUnit":"A",
            "chargingSchedulePeriod":[{"startPeriod":0,"limit":6}]}}),
    )
    .await;
    let evidence = unusual.composite_schedule_201.as_ref().unwrap();
    assert_eq!(evidence.reason_code, None);
    assert_eq!(evidence.schedule.as_ref().unwrap().duration, 60);
    drop(coordinator);
    finish(running, store).await;
    let reopened = database.open();
    for result in [grid, rejected, unusual] {
        assert_eq!(
            reopened
                .command_result_by_request_id(result.return_route.request_id.clone())
                .await
                .unwrap(),
            Some(result)
        );
    }
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Each malformed reply is one visible table row.
async fn malformed_or_inconsistent_composite_replies_stay_uncertain_without_evidence() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let base = json!({"evseId":1,"duration":3600,"scheduleStart":"2026-09-01T02:00:00Z",
        "chargingRateUnit":"A","chargingSchedulePeriod":[{"startPeriod":0,"limit":16}]});
    let mutate = |path: &[&str], value: Value| {
        let mut schedule = base.clone();
        let mut target = &mut schedule;
        for key in &path[..path.len() - 1] {
            target = &mut target[*key];
        }
        target[path[path.len() - 1]] = value;
        json!({"status":"Accepted","schedule":schedule})
    };
    let replies = [
        json!({"status":"Accepted"}),
        json!({"status":"Unknown","schedule":base}),
        json!({"status":"Accepted","schedule":base,"extra":true}),
        mutate(&["evseId"], json!(2)),
        mutate(&["duration"], json!(3601)),
        mutate(&["duration"], json!(0)),
        mutate(&["chargingRateUnit"], json!("W")),
        mutate(&["scheduleStart"], json!("not-a-time")),
        mutate(&["chargingSchedulePeriod"], json!([])),
        mutate(
            &["chargingSchedulePeriod"],
            json!([{"startPeriod":5,"limit":1}]),
        ),
        mutate(
            &["chargingSchedulePeriod"],
            json!([{"startPeriod":0,"limit":1},{"startPeriod":0,"limit":2}]),
        ),
        mutate(
            &["chargingSchedulePeriod"],
            json!([{"startPeriod":0,"limit":1},{"startPeriod":3600,"limit":2}]),
        ),
        mutate(
            &["chargingSchedulePeriod"],
            raw(r#"[{"startPeriod":0,"limit":8.15}]"#),
        ),
        mutate(
            &["chargingSchedulePeriod"],
            json!([{"startPeriod":0,"limit":-1}]),
        ),
        mutate(
            &["chargingSchedulePeriod"],
            json!([{"startPeriod":0,"limit":1,"numberPhases":4}]),
        ),
        mutate(
            &["chargingSchedulePeriod"],
            json!([{"startPeriod":0,"limit":1,"phaseToUse":1}]),
        ),
        mutate(
            &["chargingSchedulePeriod"],
            json!([{"startPeriod":0,"limit":1,"unknown":1}]),
        ),
    ];
    for (index, reply) in replies.into_iter().enumerate() {
        let request = query(
            &state,
            &format!("malformed-{index}"),
            "GetCompositeSchedule",
            json!({"evseId":1,"duration":3600,"chargingRateUnit":"A"}),
            false,
        );
        let (_, result) = exchange(&mut running, &coordinator, request, reply).await;
        uncertain(&result);
    }
    let pending = submit(
        &coordinator,
        query(
            &state,
            "callerror",
            "GetCompositeSchedule",
            json!({"evseId":0,"duration":60}),
            true,
        ),
    );
    let call = receive_json(&mut running.peer).await;
    running
        .peer
        .send_text(json!([4, call[1], "NotSupported", "private native detail", {}]).to_string())
        .await
        .unwrap();
    let result = pending.await.unwrap();
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
    assert!(result.composite_schedule_201.is_none());
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("private native detail")
    );
    finish(running, store).await;
}

#[tokio::test]
async fn delayed_composite_reply_keeps_heartbeats_and_reconnect_never_replays() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let request = query(
        &state,
        "slow",
        "GetCompositeSchedule",
        json!({"evseId":1,"duration":60}),
        false,
    );
    let pending = submit(&coordinator, request.clone());
    assert_eq!(receive_json(&mut running.peer).await[1], "slow");
    running
        .peer
        .send_text(json!([2, "heartbeat-during-query", "Heartbeat", {}]).to_string())
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
    assert_eq!(
        receive_json(&mut running.peer).await[1],
        "heartbeat-during-query"
    );
    assert!(!pending.is_finished());
    assert_eq!(
        coordinator.submit(request.clone()).await.unwrap().lifecycle,
        CommandLifecycle::Dispatched
    );
    no_call(&mut running).await;
    running.peer.disconnect().await.unwrap();
    let result = pending.await.unwrap();
    uncertain(&result);
    drop(coordinator);
    finish(running, store).await;

    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let store = database.open();
    let (_, _, coordinator) = configured(&store, &running).await;
    assert_eq!(coordinator.submit(request).await.unwrap(), result);
    no_call(&mut running).await;
    finish(running, store).await;
}
