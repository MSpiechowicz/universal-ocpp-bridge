use super::*;

/// Drives one fixture CALL through admission and answers with the paired fixture reply.
async fn replay(
    running: &mut RunningSession,
    coordinator: &Arc<Coordinator>,
    state: &StationSnapshot,
    call: &str,
    reply: &str,
    id: Option<&str>,
) -> CommandResult {
    let call = fixture(call);
    let station = call[3]["evseId"].as_i64().is_none_or(|evse| evse == 0);
    let action = call[2].as_str().unwrap();
    let id = id.unwrap_or_else(|| call[1].as_str().unwrap());
    let pending = submit(
        coordinator,
        query(state, id, action, call[3].clone(), station),
    );
    let sent = receive_json(&mut running.peer).await;
    assert_eq!((&sent[2], &sent[3]), (&call[2], &call[3]));
    let reply = fixture(reply);
    running
        .peer
        .send_text(json!([3, sent[1], reply[2]]).to_string())
        .await
        .unwrap();
    pending.await.unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Every corpus fixture is replayed on one authenticated socket.
async fn independent_k08_and_k09_fixtures_drive_actual_socket_exchanges() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let grid = replay(
        &mut running,
        &coordinator,
        &state,
        "composite-schedule-201-grid",
        "composite-schedule-201-accepted",
        None,
    )
    .await;
    let periods = &grid
        .composite_schedule_201
        .as_ref()
        .unwrap()
        .schedule
        .as_ref()
        .unwrap()
        .charging_schedule_period;
    assert_eq!(
        periods
            .iter()
            .map(|p| p.limit.to_string())
            .collect::<Vec<_>>(),
        ["22000", "7360.5", "0"]
    );
    let phase = replay(
        &mut running,
        &coordinator,
        &state,
        "composite-schedule-201-evse",
        "composite-schedule-201-phase",
        None,
    )
    .await;
    let schedule = phase
        .composite_schedule_201
        .as_ref()
        .unwrap()
        .schedule
        .as_ref()
        .unwrap();
    assert_eq!(schedule.schedule_start, time("2026-09-01T00:00:00Z"));
    assert_eq!(schedule.charging_schedule_period[0].phase_to_use, Some(3));
    let rejected = replay(
        &mut running,
        &coordinator,
        &state,
        "composite-schedule-201-evse",
        "composite-schedule-201-rejected",
        Some("k08-rejected"),
    )
    .await;
    let evidence = rejected.composite_schedule_201.as_ref().unwrap();
    assert_eq!(
        evidence.reason_code,
        Some(SmartChargingReason201::UnsupportedRateUnit)
    );
    // The original request ID returns the stored result without another CALL.
    let duplicate = fixture("composite-schedule-201-evse");
    let again = coordinator
        .submit(query(
            &state,
            "independent-k08-evse",
            "GetCompositeSchedule",
            duplicate[3].clone(),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(again, phase);
    no_call(&mut running).await;
    let none = replay(
        &mut running,
        &coordinator,
        &state,
        "charging-profiles-201-ids",
        "charging-profiles-201-no-profiles",
        None,
    )
    .await;
    assert_eq!(
        none.charging_profiles_201.as_ref().unwrap().report,
        ChargingProfileReportState201::NotExpected
    );
    let accepted_report = replay(
        &mut running,
        &coordinator,
        &state,
        "charging-profiles-201-sources",
        "charging-profiles-201-accepted",
        None,
    )
    .await;
    accepted(&accepted_report);
    let ack = fixture("report-charging-profiles-201-ack");
    for name in [
        "report-charging-profiles-201-station",
        "report-charging-profiles-201-evse",
    ] {
        let frame = fixture(name);
        let answer = report(&mut running, frame[1].as_str().unwrap(), &frame[3]).await;
        assert_eq!(
            (answer[0].clone(), answer[2].clone()),
            (ack[0].clone(), ack[2].clone())
        );
    }
    let settled = settled(&store, "independent-k09-sources").await;
    let ChargingProfileReportState201::Complete { profiles, .. } = report_state(&settled) else {
        panic!("complete: {settled:?}")
    };
    assert_eq!(
        profiles
            .iter()
            .map(|p| (
                p.evse_id,
                p.charging_limit_source,
                p.id,
                p.sales_tariff_omitted
            ))
            .collect::<Vec<_>>(),
        [
            (0, ChargingLimitSource201::Ems, 120, false),
            (1, ChargingLimitSource201::So, 121, true)
        ]
    );
    assert_eq!(
        profiles[1].charging_schedule[0]
            .min_charging_rate
            .as_ref()
            .unwrap()
            .to_string(),
        "6"
    );
    finish(running, store).await;
}
