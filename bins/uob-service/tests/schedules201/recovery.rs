use super::support::*;
use serde_json::json;

#[tokio::test]
async fn delayed_schedule_reply_keeps_heartbeat_progress() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let request = begin(
        &client,
        &fixture,
        command(
            "slow",
            "GetCompositeSchedule",
            json!({"evseId":1,"duration":60}),
            true,
        ),
    );
    let call = receive(&mut station).await;
    assert_eq!(call[2], "GetCompositeSchedule");
    for index in 0..3 {
        let reply = station_call(
            &mut station,
            &format!("beat-{index}"),
            "Heartbeat",
            json!({}),
        )
        .await;
        assert!(reply["currentTime"].is_string());
    }
    assert!(!request.is_finished());
    send(
        &mut station,
        json!([3, call[1], {"status":"Accepted","schedule":{"evseId":1,"duration":60,
        "scheduleStart":"2026-09-01T00:00:00Z","chargingRateUnit":"A",
        "chargingSchedulePeriod":[{"startPeriod":0,"limit":6}]}}]),
    )
    .await;
    assert_eq!(
        completed(request).await["composite_schedule_201"]["status"],
        "Accepted"
    );
}

#[tokio::test]
async fn crash_after_acknowledgement_interrupts_report_and_reconnect_never_replays() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let body = command(
        "restart-report",
        "GetChargingProfiles",
        json!({"requestId":0,"chargingProfile":{"chargingProfilePurpose":"TxProfile"}}),
        false,
    );
    let acknowledged = answer(
        &client,
        &fixture,
        &mut station,
        &body,
        r#"{"status":"Accepted"}"#,
    )
    .await;
    assert_eq!(
        acknowledged["charging_profiles_201"]["report"]["state"],
        "pending"
    );
    let lost = command(
        "lost-schedule",
        "GetCompositeSchedule",
        json!({"evseId":0,"duration":60}),
        false,
    );
    let request = begin(&client, &fixture, lost.clone());
    assert_eq!(receive(&mut station).await[2], "GetCompositeSchedule");
    drop(child);
    drop(station);
    let _ = request.await;
    let mut restarted = fixture.start();
    fixture.ready(&mut restarted).await;
    let recovered = result(&client, &fixture, "restart-report").await;
    assert_eq!(recovered["lifecycle"]["stage"], "protocol_response");
    assert_eq!(recovered["charging_profiles_201"]["status"], "Accepted");
    assert_eq!(
        recovered["charging_profiles_201"]["report"]["state"],
        "incomplete"
    );
    assert_eq!(
        recovered["charging_profiles_201"]["report"]["reason"],
        "interrupted"
    );
    let uncertain = result(&client, &fixture, "lost-schedule").await;
    assert_eq!(uncertain["lifecycle"]["stage"], "transmission_uncertain");
    assert!(uncertain["composite_schedule_201"].is_null());
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    no_call(&mut station).await;
    boot(&mut station).await;
    assert_eq!(completed(begin(&client, &fixture, body)).await, recovered);
    assert_eq!(completed(begin(&client, &fixture, lost)).await, uncertain);
    no_call(&mut station).await;
    // A late fragment for the interrupted request is acknowledged but never revives it.
    let late = report(
        &mut station,
        "late",
        json!({"requestId":0,"chargingLimitSource":"CSO","evseId":1,
        "chargingProfile":[{"id":1,"stackLevel":0,"chargingProfilePurpose":"TxProfile",
        "chargingProfileKind":"Relative","chargingSchedule":[{"id":1,"chargingRateUnit":"A",
        "chargingSchedulePeriod":[{"startPeriod":0,"limit":6}]}]}]}),
    )
    .await;
    assert_eq!(late, json!([3, "late", {}]));
    assert_eq!(result(&client, &fixture, "restart-report").await, recovered);
    // The fresh connection may reuse the native request ID for a new explicit query.
    let fresh = command(
        "fresh",
        "GetChargingProfiles",
        json!({"requestId":0,"chargingProfile":{"stackLevel":0}}),
        false,
    );
    let value = answer(
        &client,
        &fixture,
        &mut station,
        &fresh,
        r#"{"status":"NoProfiles"}"#,
    )
    .await;
    assert_eq!(value["charging_profiles_201"]["status"], "NoProfiles");
}
