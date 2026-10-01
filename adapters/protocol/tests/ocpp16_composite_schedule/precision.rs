use super::*;
#[tokio::test]
async fn exact_exponent_and_model_boundaries_never_round_native_rates() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    for (index, (rate, expected)) in [
        ("9.007199254740991e14", "900719925474099.1"),
        ("81e-1", "8.1"),
        ("8.10000000000000000000000000000000000", "8.1"),
        (
            "79228162514264337593543950335",
            "79228162514264337593543950335",
        ),
        (
            "7922816251426433759354395033.5",
            "7922816251426433759354395033.5",
        ),
        ("0e-29", "0"),
        ("-0.000", "0"),
    ]
    .into_iter()
    .enumerate()
    {
        let result = exchange(
            &mut running,
            &coordinator,
            query(&state, &format!("exact-{index}"), 1, None),
            reply(rate, "A"),
        )
        .await;
        assert!(
            matches!(
                &result.lifecycle,
                CommandLifecycle::ProtocolResponse {
                    accepted: true,
                    error: None
                }
            ),
            "rate {rate} must be accepted without rounding: {:?}",
            result.lifecycle
        );
        let evidence = result.composite_schedule_16.as_ref().unwrap();
        assert_eq!(
            evidence
                .charging_schedule
                .as_ref()
                .unwrap()
                .charging_schedule_period[0]
                .limit
                .to_string(),
            expected
        );
        durable(&store, &result).await;
    }

    let minimum = "792281625142643375935439503350e-1";
    let mut payload = reply("0", "A");
    payload["chargingSchedule"]["minChargingRate"] = serde_json::from_str(minimum).unwrap();
    let result = exchange(
        &mut running,
        &coordinator,
        query(&state, "exact-large-minimum", 1, None),
        payload,
    )
    .await;
    assert!(
        matches!(
            &result.lifecycle,
            CommandLifecycle::ProtocolResponse {
                accepted: true,
                error: None
            }
        ),
        "zero period with minimum {minimum} must be accepted exactly: {:?}",
        result.lifecycle
    );
    let schedule = result
        .composite_schedule_16
        .as_ref()
        .unwrap()
        .charging_schedule
        .as_ref()
        .unwrap();
    assert_eq!(
        schedule.charging_schedule_period[0].limit,
        ExactDecimal::new(0, 0)
    );
    assert_eq!(
        schedule.min_charging_rate,
        Some(ExactDecimal::new((1_i128 << 96) - 1, 0))
    );
    durable(&store, &result).await;
    finish(running, store).await;
}
