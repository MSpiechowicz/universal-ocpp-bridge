use super::support::*;
use serde_json::json;

#[tokio::test]
async fn composite_schedules_keep_exact_scope_and_rates_in_http_and_after_restart() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let grid = command(
        "grid",
        "GetCompositeSchedule",
        json!({"evseId":0,"duration":86400,"chargingRateUnit":"W"}),
        false,
    );
    let accepted = answer(&client, &fixture, &mut station, &grid,
        r#"{"status":"Accepted","schedule":{"evseId":0,"duration":86400,
        "scheduleStart":"2026-09-01T00:00:00Z","chargingRateUnit":"W","chargingSchedulePeriod":[
        {"startPeriod":0,"limit":900719925474099.1},{"startPeriod":60,"limit":0,"numberPhases":1,"phaseToUse":2}]}}"#,
    )
    .await;
    assert_eq!(accepted["lifecycle"]["stage"], "protocol_response");
    assert_eq!(accepted["lifecycle"]["accepted"], true);
    assert_eq!(accepted["schema_version"], json!({"major":1,"revision":13}));
    let periods = &accepted["composite_schedule_201"]["schedule"]["charging_schedule_period"];
    assert_eq!(periods[0]["limit"], "900719925474099.1");
    assert_eq!(periods[1]["limit"], "0");
    assert_eq!(periods[1]["phase_to_use"], 2);
    let evse = command(
        "evse",
        "GetCompositeSchedule",
        json!({"evseId":1,"duration":60}),
        true,
    );
    let rejected = answer(&client, &fixture, &mut station, &evse,
        r#"{"status":"Rejected","statusInfo":{"reasonCode":"UnknownEvse","additionalInfo":"PRIVATE-NATIVE"}}"#,
    )
    .await;
    assert_eq!(rejected["lifecycle"]["accepted"], false);
    assert_eq!(rejected["lifecycle"]["error"]["code"], "protocol_rejected");
    assert_eq!(rejected["composite_schedule_201"]["status"], "Rejected");
    assert_eq!(
        rejected["composite_schedule_201"]["reason_code"],
        "UnknownEvse"
    );
    assert!(!rejected.to_string().contains("PRIVATE-NATIVE"));
    drop(child);
    drop(station);
    let mut restarted = fixture.start();
    fixture.ready(&mut restarted).await;
    assert_eq!(result(&client, &fixture, "grid").await, accepted);
    assert_eq!(result(&client, &fixture, "evse").await, rejected);
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    no_call(&mut station).await;
}

#[tokio::test]
async fn installed_profile_reports_complete_through_http_after_the_acknowledgement() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let body = command(
        "profiles",
        "GetChargingProfiles",
        json!({"requestId":-120,"chargingProfile":{"chargingLimitSource":["EMS","CSO"]}}),
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
    assert_eq!(acknowledged["lifecycle"]["accepted"], true);
    assert_eq!(acknowledged["charging_profiles_201"]["status"], "Accepted");
    assert_eq!(
        acknowledged["charging_profiles_201"]["report"]["state"],
        "pending"
    );
    let profile = |id: i32, purpose: &str| {
        json!({"id":id,"stackLevel":0,"chargingProfilePurpose":purpose,"chargingProfileKind":"Absolute",
            "customData":{"vendorId":"PRIVATE-VENDOR"},
            "chargingSchedule":[{"id":id,"startSchedule":"2026-09-01T00:00:00Z","chargingRateUnit":"A",
            "chargingSchedulePeriod":[{"startPeriod":0,"limit":32}]}]})
    };
    let first = report(
        &mut station,
        "r-0",
        json!({"requestId":-120,"chargingLimitSource":"CSO","tbc":true,
        "evseId":0,"chargingProfile":[profile(10, "ChargingStationMaxProfile")]}),
    )
    .await;
    assert_eq!(first, json!([3, "r-0", {}]));
    let second = report(
        &mut station,
        "r-1",
        json!({"requestId":-120,"chargingLimitSource":"EMS",
        "evseId":1,"chargingProfile":[profile(11, "ChargingStationExternalConstraints")]}),
    )
    .await;
    assert_eq!(second, json!([3, "r-1", {}]));
    let completed = terminal(&client, &fixture, "profiles").await;
    let report = &completed["charging_profiles_201"]["report"];
    assert_eq!(report["state"], "complete");
    assert_eq!(report["progress"]["fragments"], 2);
    let profiles = report["profiles"].as_array().unwrap();
    assert_eq!(
        profiles
            .iter()
            .map(|p| (
                p["evse_id"].clone(),
                p["charging_limit_source"].clone(),
                p["id"].clone()
            ))
            .collect::<Vec<_>>(),
        [
            (json!(0), json!("CSO"), json!(10)),
            (json!(1), json!("EMS"), json!(11))
        ]
    );
    assert!(!completed.to_string().contains("PRIVATE-VENDOR"));
    // NoProfiles is terminal without a report; the lifecycle records no success claim.
    let none = command(
        "none",
        "GetChargingProfiles",
        json!({"requestId":7,"evseId":1,"chargingProfile":{"chargingProfileId":[99]}}),
        true,
    );
    let empty = answer(
        &client,
        &fixture,
        &mut station,
        &none,
        r#"{"status":"NoProfiles"}"#,
    )
    .await;
    assert_eq!(empty["lifecycle"]["accepted"], false);
    assert_eq!(
        empty["charging_profiles_201"]["report"]["state"],
        "not_expected"
    );
    drop(child);
    let mut restarted = fixture.start();
    fixture.ready(&mut restarted).await;
    assert_eq!(result(&client, &fixture, "profiles").await, completed);
}
