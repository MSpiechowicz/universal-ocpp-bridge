use super::*;

#[tokio::test]
async fn exactly_1024_periods_and_signed_identity_boundaries_are_preserved() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    for (index, id) in [i32::MIN, i32::MAX].into_iter().enumerate() {
        let mut payload = set_payload(0, "0", "W");
        payload["csChargingProfiles"]["chargingProfileId"] = json!(id);
        payload["csChargingProfiles"]["stackLevel"] = json!(i32::MAX);
        payload["csChargingProfiles"]["chargingProfilePurpose"] = json!("ChargePointMaxProfile");
        payload["csChargingProfiles"]["chargingSchedule"]["chargingSchedulePeriod"] = json!(
            (0..1024)
                .map(|index| json!({"startPeriod":index,"limit":0,"numberPhases":i32::MAX}))
                .collect::<Vec<_>>()
        );
        let result = exchange(
            &mut running,
            &coordinator,
            request(
                &state,
                &format!("boundary-{index}"),
                "SetChargingProfile",
                payload,
                true,
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
        assert_eq!(request.cs_charging_profiles.charging_profile_id, id);
        assert_eq!(
            request
                .cs_charging_profiles
                .charging_schedule
                .charging_schedule_period
                .len(),
            1024
        );
        assert_eq!(
            request
                .cs_charging_profiles
                .charging_schedule
                .charging_schedule_period[1023]
                .start_period,
            1023
        );
    }
    finish(running, store).await;
}

#[tokio::test]
async fn native_optional_recurrence_and_expired_validity_remain_charger_decisions() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;

    let mut payload = set_payload(1, "0", "A");
    let profile = &mut payload["csChargingProfiles"];
    profile.as_object_mut().unwrap().remove("recurrencyKind");
    profile["validFrom"] = json!("2026-09-03T00:00:00Z");
    profile["validTo"] = json!("2026-09-01T00:00:00Z");
    profile["chargingSchedule"]["duration"] = json!(0);

    let result = exchange(
        &mut running,
        &coordinator,
        request(
            &state,
            "native-optional",
            "SetChargingProfile",
            payload,
            false,
        ),
        json!({"status":"Accepted"}),
    )
    .await;
    accepted(&result);
    let Some(ChargingProfileResult16::SetChargingProfile { request, .. }) =
        result.charging_profile_16
    else {
        panic!("missing native profile evidence");
    };
    let profile = request.cs_charging_profiles;
    assert_eq!(profile.recurrency_kind, None);
    assert_eq!(profile.charging_schedule.duration, Some(0));
    assert_eq!(
        profile.valid_from,
        Some(serde_json::from_value::<UtcTimestamp>(json!("2026-09-03T00:00:00Z")).unwrap())
    );
    assert_eq!(
        profile.valid_to,
        Some(serde_json::from_value::<UtcTimestamp>(json!("2026-09-01T00:00:00Z")).unwrap())
    );
    assert_eq!(
        profile.charging_schedule.charging_schedule_period[1].start_period,
        86401
    );

    finish(running, store).await;
}
