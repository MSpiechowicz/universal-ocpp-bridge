use super::*;

const EXTERNAL: &str = r#"{"id":-7,"stackLevel":3,"chargingProfilePurpose":"ChargingStationExternalConstraints",
    "chargingProfileKind":"Recurring","recurrencyKind":"Weekly","validFrom":"2026-09-01T00:00:00Z",
    "validTo":"2026-10-01T00:00:00Z","customData":{"vendorId":"v","private":"opaque-vendor"},
    "chargingSchedule":[
        {"id":1,"duration":604800,"startSchedule":"2026-08-31T00:00:00Z","chargingRateUnit":"W",
         "chargingSchedulePeriod":[{"startPeriod":0,"limit":11000.5,"numberPhases":3},
                                   {"startPeriod":3600,"limit":0}],"minChargingRate":1.0e3},
        {"id":2,"chargingRateUnit":"A","chargingSchedulePeriod":[{"startPeriod":0,"limit":6}],
         "salesTariff":{"id":1,"salesTariffEntry":[{"relativeTimeInterval":{"start":0}}]}}]}"#;

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep acknowledgement, fragments and the reopened report together.
async fn accepted_reports_retain_sources_schedules_and_arrival_order_after_reopen() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let request = query(
        &state,
        "all-sources",
        "GetChargingProfiles",
        json!({"requestId":i32::MIN,"chargingProfile":{"chargingLimitSource":["CSO","EMS"]}}),
        true,
    );
    let (call, acknowledged) = exchange(
        &mut running,
        &coordinator,
        request,
        json!({"status":"Accepted","statusInfo":{"reasonCode":"NoError","additionalInfo":"private"}}),
    )
    .await;
    assert_eq!(call[2], "GetChargingProfiles");
    accepted(&acknowledged);
    assert_eq!(
        acknowledged.schema_version,
        ContractVersion::V1_SCHEDULES_201
    );
    let evidence = acknowledged.charging_profiles_201.as_ref().unwrap();
    assert_eq!(evidence.status, ChargingProfilesStatus201::Accepted);
    assert_eq!(evidence.reason_code, None);
    assert_eq!(
        evidence.query.charging_limit_source,
        [ChargingLimitSource201::Cso, ChargingLimitSource201::Ems]
    );
    assert_eq!(evidence.query.evse_id, None);
    assert!(evidence.report.pending());

    let first = fragment(
        i32::MIN,
        0,
        "CSO",
        true,
        &[
            profile(1, "ChargingStationMaxProfile", 0),
            profile(2, "TxDefaultProfile", 1),
        ],
    );
    assert_eq!(
        report(&mut running, "report-0", &first).await,
        json!([3, "report-0", {}])
    );
    let second = fragment(i32::MIN, 1, "EMS", false, &[raw(EXTERNAL)]);
    assert_eq!(
        report(&mut running, "report-1", &second).await,
        json!([3, "report-1", {}])
    );
    let settled = settled(&store, "all-sources").await;
    let ChargingProfileReportState201::Complete {
        progress,
        fragments,
        profiles,
    } = report_state(&settled)
    else {
        panic!("complete report: {settled:?}")
    };
    assert_eq!((progress.fragments, progress.profiles), (2, 3));
    assert_eq!(
        fragments
            .iter()
            .map(|f| (
                f.sequence,
                f.evse_id,
                f.charging_limit_source,
                f.more,
                f.profiles
            ))
            .collect::<Vec<_>>(),
        [
            (0, 0, ChargingLimitSource201::Cso, true, 2),
            (1, 1, ChargingLimitSource201::Ems, false, 1)
        ]
    );
    assert_eq!(
        profiles
            .iter()
            .map(|p| (p.evse_id, p.id))
            .collect::<Vec<_>>(),
        [(0, 1), (0, 2), (1, -7)]
    );
    let external = &profiles[2];
    assert_eq!(
        external.charging_profile_purpose,
        ReportedChargingProfilePurpose201::ChargingStationExternalConstraints
    );
    assert_eq!(external.charging_limit_source, ChargingLimitSource201::Ems);
    assert_eq!(
        external.recurrency_kind,
        Some(ChargingProfileRecurrency201::Weekly)
    );
    assert!(external.sales_tariff_omitted && !profiles[0].sales_tariff_omitted);
    let schedules = &external.charging_schedule;
    assert_eq!(schedules.len(), 2);
    assert_eq!(
        schedules[0].charging_schedule_period[0].limit.to_string(),
        "11000.5"
    );
    assert_eq!(
        schedules[0].min_charging_rate.as_ref().unwrap().to_string(),
        "1000"
    );
    assert_eq!(
        schedules[1].charging_rate_unit,
        ChargingScheduleRateUnit201::A
    );
    let encoded = serde_json::to_string(&settled).unwrap();
    assert!(!encoded.contains("opaque-vendor") && !encoded.contains("salesTariffEntry"));
    assert!(!encoded.contains("private"));
    // The original submission and an exact duplicate return the same durable identity.
    let duplicate = coordinator
        .submit(query(
            &state,
            "all-sources",
            "GetChargingProfiles",
            json!({"requestId":i32::MIN,"chargingProfile":{"chargingLimitSource":["CSO","EMS"]}}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(duplicate, settled);
    no_call(&mut running).await;
    drop(coordinator);
    finish(running, store).await;
    let reopened = database.open();
    assert_eq!(
        reopened
            .command_result_by_request_id(RequestId::new("all-sources").unwrap())
            .await
            .unwrap(),
        Some(settled)
    );
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn early_fragments_wait_for_the_reply_and_no_profiles_ends_without_a_report() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let pending = submit(
        &coordinator,
        query(
            &state,
            "early",
            "GetChargingProfiles",
            json!({"requestId":5,"evseId":1,"chargingProfile":{"chargingProfileId":[1,2]}}),
            false,
        ),
    );
    let call = receive_json(&mut running.peer).await;
    let early = fragment(5, 1, "CSO", false, &[profile(2, "TxProfile", 0)]);
    assert_eq!(
        report(&mut running, "early-0", &early).await,
        json!([3, "early-0", {}])
    );
    running
        .peer
        .send_text(json!([3, call[1], {"status":"Accepted"}]).to_string())
        .await
        .unwrap();
    let result = pending.await.unwrap();
    accepted(&result);
    let ChargingProfileReportState201::Complete { profiles, .. } = report_state(&result) else {
        panic!("report completed before the reply is recorded with it: {result:?}")
    };
    assert_eq!(profiles[0].id, 2);
    assert_eq!(settled(&store, "early").await, result);

    let (_, none) = exchange(
        &mut running,
        &coordinator,
        query(
            &state,
            "none",
            "GetChargingProfiles",
            json!({"requestId":6,"evseId":0,"chargingProfile":{"stackLevel":0}}),
            true,
        ),
        json!({"status":"NoProfiles","statusInfo":{"reasonCode":"notfound"}}),
    )
    .await;
    assert!(matches!(
        none.lifecycle,
        CommandLifecycle::ProtocolResponse {
            accepted: false,
            error: None
        }
    ));
    let evidence = none.charging_profiles_201.as_ref().unwrap();
    assert_eq!(evidence.status, ChargingProfilesStatus201::NoProfiles);
    assert_eq!(evidence.reason_code, Some(SmartChargingReason201::NotFound));
    assert_eq!(evidence.report, ChargingProfileReportState201::NotExpected);
    // A stray fragment after NoProfiles is acknowledged on the wire but never retained.
    let stray = fragment(6, 0, "CSO", false, &[profile(9, "TxDefaultProfile", 0)]);
    assert_eq!(
        report(&mut running, "stray", &stray).await,
        json!([3, "stray", {}])
    );
    assert_eq!(
        store
            .command_result_by_request_id(RequestId::new("none").unwrap())
            .await
            .unwrap(),
        Some(none)
    );
    finish(running, store).await;
}

#[tokio::test]
async fn out_of_query_malformed_and_disconnected_reports_end_incomplete() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let cases = [
        // K09.FR.04: a selected EVSE bounds every fragment.
        (
            fragment(11, 2, "CSO", false, &[profile(1, "TxProfile", 0)]),
            json!([3, "f-11", {}]),
            ChargingProfileReportFailure201::Correlation,
        ),
        // Unrequested profile IDs and limit sources are not part of this report.
        (
            fragment(12, 1, "CSO", false, &[profile(3, "TxProfile", 0)]),
            json!([3, "f-12", {}]),
            ChargingProfileReportFailure201::Correlation,
        ),
        (
            json!({"requestId":13,"chargingLimitSource":"CSO","evseId":1,"chargingProfile":[]}),
            Value::Null,
            ChargingProfileReportFailure201::InvalidFragment,
        ),
    ];
    for (index, (body, answer, reason)) in cases.into_iter().enumerate() {
        let request_id = 11 + i32::try_from(index).unwrap();
        let id = format!("bad-{request_id}");
        let (_, acknowledged) = exchange(
            &mut running,
            &coordinator,
            query(
                &state,
                &id,
                "GetChargingProfiles",
                json!({"requestId":request_id,"evseId":1,"chargingProfile":{"chargingProfileId":[1,2]}}),
                false,
            ),
            json!({"status":"Accepted"}),
        )
        .await;
        assert!(acknowledged.charging_profiles_201.is_some());
        let frame = report(&mut running, &format!("f-{request_id}"), &body).await;
        if answer.is_null() {
            assert_eq!(frame[0], 4);
        } else {
            assert_eq!(frame, answer);
        }
        let result = settled(&store, &id).await;
        assert!(matches!(
            report_state(&result),
            ChargingProfileReportState201::Incomplete { reason: actual, .. } if *actual == reason
        ));
    }
    let (_, acknowledged) = exchange(
        &mut running,
        &coordinator,
        query(
            &state,
            "cut",
            "GetChargingProfiles",
            json!({"requestId":20,"chargingProfile":{"chargingProfilePurpose":"TxProfile"}}),
            true,
        ),
        json!({"status":"Accepted"}),
    )
    .await;
    assert!(acknowledged.charging_profiles_201.is_some());
    let partial = fragment(20, 1, "CSO", true, &[profile(4, "TxProfile", 0)]);
    assert_eq!(
        report(&mut running, "cut-0", &partial).await,
        json!([3, "cut-0", {}])
    );
    running.peer.disconnect().await.unwrap();
    let result = settled(&store, "cut").await;
    let ChargingProfileReportState201::Incomplete { reason, progress } = report_state(&result)
    else {
        panic!("incomplete: {result:?}")
    };
    assert_eq!(*reason, ChargingProfileReportFailure201::Disconnected);
    assert_eq!(progress.map(|p| (p.fragments, p.profiles)), Some((1, 1)));
    accepted(&result);
    finish(running, store).await;
}
