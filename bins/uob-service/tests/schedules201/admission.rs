use super::support::*;
use serde_json::json;

#[tokio::test]
async fn default_off_unprivileged_and_out_of_scope_queries_never_reach_native_socket() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    let mut disabled = fixture.station_protocol("station-b", "ocpp2.0.1").await;
    boot(&mut station).await;
    boot(&mut disabled).await;
    let composite = command(
        "authority",
        "GetCompositeSchedule",
        json!({"evseId":0,"duration":60}),
        false,
    );
    assert_eq!(status(&client, &fixture, CONTROL, &composite).await, 403);
    let reports = command(
        "authority-k09",
        "GetChargingProfiles",
        json!({"requestId":1,"chargingProfile":{"stackLevel":0}}),
        false,
    );
    assert_eq!(status(&client, &fixture, CONTROL, &reports).await, 403);
    no_call(&mut station).await;
    for (id, action, payload) in [
        (
            "off-k08",
            "GetCompositeSchedule",
            json!({"evseId":0,"duration":60}),
        ),
        (
            "off-k09",
            "GetChargingProfiles",
            json!({"requestId":1,"chargingProfile":{"stackLevel":0}}),
        ),
    ] {
        let mut off = command(id, action, payload, false);
        off["resource"]["station_id"] = json!("station-b");
        assert_eq!(status(&client, &fixture, PRIVILEGED, &off).await, 422);
    }
    no_call(&mut disabled).await;
    // The default-off station has no report namespace either.
    send(&mut disabled, json!([2, "unsolicited", "ReportChargingProfiles",
        {"requestId":1,"chargingLimitSource":"CSO","evseId":0,"chargingProfile":[{"id":1,"stackLevel":0,
        "chargingProfilePurpose":"TxDefaultProfile","chargingProfileKind":"Relative",
        "chargingSchedule":[{"id":1,"chargingRateUnit":"A","chargingSchedulePeriod":[{"startPeriod":0,"limit":6}]}]}]}])).await;
    let refused = receive(&mut disabled).await;
    assert_eq!(
        (refused[0].clone(), refused[2].clone()),
        (json!(4), json!("NotImplemented"))
    );
    for (id, action, payload, evse) in [
        (
            "grid-at-evse",
            "GetCompositeSchedule",
            json!({"evseId":0,"duration":60}),
            true,
        ),
        (
            "evse-at-grid",
            "GetCompositeSchedule",
            json!({"evseId":1,"duration":60}),
            false,
        ),
        (
            "zero-duration",
            "GetCompositeSchedule",
            json!({"evseId":0,"duration":0}),
            false,
        ),
        (
            "opaque",
            "GetCompositeSchedule",
            json!({"evseId":0,"duration":60,"customData":{"vendorId":"SECRET-REQUEST"}}),
            false,
        ),
        (
            "both-criteria",
            "GetChargingProfiles",
            json!({"requestId":2,"chargingProfile":{"chargingProfileId":[1],"stackLevel":0}}),
            false,
        ),
        (
            "other-evse",
            "GetChargingProfiles",
            json!({"requestId":3,"evseId":2,"chargingProfile":{"stackLevel":0}}),
            true,
        ),
    ] {
        let code = status(
            &client,
            &fixture,
            PRIVILEGED,
            &command(id, action, payload, evse),
        )
        .await;
        assert_eq!(code, 400, "{id}");
    }
    no_call(&mut station).await;
}
