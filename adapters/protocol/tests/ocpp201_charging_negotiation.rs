use serde_json::{Value, json};
use uob_application::{ChargerObservation, NegotiationObservation201};
use uob_contracts::*;
use uob_protocol_adapter::v201::{self, charging_negotiation};

fn corpus(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/ocpp-fixtures/corpus")
            .join(name),
    )
    .unwrap()
}

fn wire(name: &str) -> Value {
    serde_json::from_slice(&corpus(&format!("wire/2.0.1/{name}.json"))).unwrap()
}

fn decoded(frame: &Value) -> Result<NegotiationObservation201, uob_protocol_adapter::DecodeError> {
    let call = v201::decode_call(frame.to_string().as_bytes())?;
    assert_eq!(call.action.as_str(), frame[2].as_str().unwrap());
    match call.observation {
        ChargerObservation::ChargingNegotiation201(observation) => Ok(observation),
        other => panic!("unexpected observation {other:?}"),
    }
}

fn decimal(value: &str) -> ExactDecimal {
    value.parse().unwrap()
}

fn period(start: i32, limit: &str, phases: Option<i32>) -> ChargingSchedulePeriod201 {
    ChargingSchedulePeriod201 {
        start_period: start,
        limit: decimal(limit),
        number_phases: phases,
        phase_to_use: None,
    }
}

fn instant(value: &str) -> UtcTimestamp {
    serde_json::from_value(json!(value)).unwrap()
}

#[test]
fn independent_charging_needs_decode_to_exact_native_parameters() {
    let NegotiationObservation201::EvChargingNeeds(ac) =
        decoded(&wire("ev-charging-needs-ac")).unwrap()
    else {
        panic!("needs");
    };
    assert_eq!(
        ac,
        EvChargingNeeds201 {
            evse_id: 1,
            max_schedule_tuples: Some(3),
            requested_energy_transfer: EnergyTransferMode201::AcThreePhase,
            departure_time: Some(instant("2026-10-06T18:30:00Z")),
            parameters: EvChargingParameters201::Ac(AcChargingParameters201 {
                energy_amount: 22_000,
                ev_min_current: 6,
                ev_max_current: 32,
                ev_max_voltage: 400,
            }),
        }
    );
    let NegotiationObservation201::EvChargingNeeds(dc) =
        decoded(&wire("ev-charging-needs-dc")).unwrap()
    else {
        panic!("needs");
    };
    assert_eq!(dc.max_schedule_tuples, None);
    assert_eq!(
        dc.parameters,
        EvChargingParameters201::Dc(DcChargingParameters201 {
            ev_max_current: 200,
            ev_max_voltage: 800,
            energy_amount: Some(45_000),
            ev_max_power: Some(150_000),
            state_of_charge: Some(35),
            ev_energy_capacity: Some(77_000),
            full_soc: Some(95),
            bulk_soc: Some(80),
        })
    );
}

#[test]
fn independent_schedules_and_limits_keep_exact_tenths_zero_and_signed_ids() {
    let NegotiationObservation201::EvChargingSchedule(schedule) =
        decoded(&wire("ev-charging-schedule")).unwrap()
    else {
        panic!("schedule");
    };
    assert_eq!(schedule.evse_id, 1);
    assert_eq!(schedule.time_base, instant("2026-10-06T17:00:00Z"));
    assert_eq!(schedule.period_count, 3);
    assert!(!schedule.sales_tariff_omitted);
    let native = schedule.charging_schedule.unwrap();
    assert_eq!((native.id, native.duration), (7, Some(7200)));
    assert_eq!(
        native.charging_schedule_period,
        vec![
            period(0, "16.0", Some(3)),
            period(1800, "10.5", Some(3)),
            period(3600, "0", None)
        ]
    );
    let NegotiationObservation201::ChargingLimit(grid) =
        decoded(&wire("charging-limit-grid")).unwrap()
    else {
        panic!("limit");
    };
    assert_eq!(
        grid.evse_id, None,
        "absent evseId addresses the grid connection"
    );
    assert_eq!(grid.charging_limit_source, ChargingLimitSource201::So);
    assert_eq!(grid.is_grid_critical, Some(true));
    let [schedule] = grid.charging_schedule.as_slice() else {
        panic!("one schedule");
    };
    assert_eq!(schedule.id, -121);
    assert_eq!(
        schedule.start_schedule,
        Some(instant("2026-10-06T17:00:00Z"))
    );
    assert_eq!(schedule.charging_rate_unit, ChargingScheduleRateUnit201::W);
    assert_eq!(
        serde_json::to_value(&schedule.charging_schedule_period).unwrap(),
        json!([{"start_period":0,"limit":"11000"},{"start_period":900,"limit":"7400.5"},
            {"start_period":2700,"limit":"0"}])
    );
    let NegotiationObservation201::ChargingLimit(evse) =
        decoded(&wire("charging-limit-evse")).unwrap()
    else {
        panic!("limit");
    };
    assert_eq!(
        (
            evse.evse_id,
            evse.is_grid_critical,
            evse.charging_schedule.len()
        ),
        (Some(1), None, 0),
        "EnableNotifyChargingLimitWithSchedules=false sends no schedule"
    );
    for (name, source, evse_id) in [
        (
            "cleared-charging-limit-grid",
            ChargingLimitSource201::So,
            None,
        ),
        (
            "cleared-charging-limit-evse",
            ChargingLimitSource201::Ems,
            Some(1),
        ),
    ] {
        assert_eq!(
            decoded(&wire(name)).unwrap(),
            NegotiationObservation201::ChargingLimitCleared(ClearedChargingLimit201 {
                charging_limit_source: source,
                evse_id,
            })
        );
    }
}

#[test]
fn schema_valid_but_natively_invalid_notifications_are_property_violations() {
    let cases: Value = serde_json::from_slice(&corpus(
        "wire/2.0.1/charging-negotiation-negative-cases.json",
    ))
    .unwrap();
    for case in cases["cases"].as_array().unwrap() {
        let error = decoded(&case["wire"]).expect_err(case["id"].as_str().unwrap());
        assert_eq!(
            error.call_error().code.as_str(),
            "PropertyConstraintViolation",
            "{}",
            case["id"]
        );
        assert_eq!(case["native_valid"], false);
    }
    // K11.FR.05/K12.FR.04 apply in every scope and with or without schedules.
    for mut frame in [wire("charging-limit-grid"), wire("charging-limit-evse")] {
        assert!(decoded(&frame).is_ok());
        frame[3]["chargingLimit"]["chargingLimitSource"] = json!("CSO");
        assert!(decoded(&frame).is_err(), "{}", frame[1]);
    }
    let cleared = json!([2,"cso","ClearedChargingLimit",{"chargingLimitSource":"CSO","evseId":0}]);
    assert_eq!(
        decoded(&cleared).unwrap(),
        NegotiationObservation201::ChargingLimitCleared(ClearedChargingLimit201 {
            charging_limit_source: ChargingLimitSource201::Cso,
            evse_id: Some(0),
        }),
        "no requirement forbids reporting a released CSO limit; zero stays as reported"
    );
}

#[test]
fn extensions_and_tariffs_are_never_retained() {
    let mut frame = wire("ev-charging-schedule");
    frame[3]["customData"] = json!({"vendorId":"independent","secret":"marker"});
    frame[3]["chargingSchedule"]["salesTariff"] = json!({"id":1,"salesTariffEntry":[
        {"relativeTimeInterval":{"start":0},"ePriceLevel":1}]});
    let NegotiationObservation201::EvChargingSchedule(schedule) = decoded(&frame).unwrap() else {
        panic!("schedule");
    };
    assert!(schedule.sales_tariff_omitted);
    let encoded = serde_json::to_string(&schedule).unwrap();
    assert!(!encoded.contains("marker") && !encoded.contains("vendorId"));
    let mut frame = wire("charging-limit-grid");
    frame[3]["chargingSchedule"][0]["salesTariff"] =
        json!({"id":2,"salesTariffEntry":[{"relativeTimeInterval":{"start":0}}]});
    frame[3]["chargingLimit"]["customData"] = json!({"vendorId":"independent"});
    let NegotiationObservation201::ChargingLimit(limit) = decoded(&frame).unwrap() else {
        panic!("limit");
    };
    assert!(limit.sales_tariff_omitted);
    assert!(
        !serde_json::to_string(&limit)
            .unwrap()
            .contains("independent")
    );
}

#[test]
fn native_answers_match_the_independent_response_fixtures() {
    let needs = |status, reason| ChargingNegotiation201::EvChargingNeeds {
        needs: EvChargingNeeds201 {
            evse_id: 1,
            max_schedule_tuples: None,
            requested_energy_transfer: EnergyTransferMode201::Dc,
            departure_time: None,
            parameters: EvChargingParameters201::Dc(DcChargingParameters201 {
                ev_max_current: 1,
                ev_max_voltage: 1,
                energy_amount: None,
                ev_max_power: None,
                state_of_charge: None,
                ev_energy_capacity: None,
                full_soc: None,
                bulk_soc: None,
            }),
        },
        status,
        reason,
        transaction_id: None,
    };
    let schedule = |status, basis, reason| ChargingNegotiation201::EvChargingSchedule {
        schedule: EvChargingSchedule201 {
            evse_id: 1,
            time_base: instant("2026-10-06T17:00:00Z"),
            charging_schedule: None,
            period_count: 1,
            sales_tariff_omitted: false,
        },
        status,
        basis,
        reason,
        transaction_id: None,
    };
    let limit = ChargingNegotiation201::ChargingLimit {
        limit: ExternalChargingLimit201 {
            evse_id: None,
            charging_limit_source: ChargingLimitSource201::So,
            is_grid_critical: None,
            charging_schedule: Vec::new(),
            schedules_omitted: None,
            sales_tariff_omitted: false,
        },
    };
    let cleared = ChargingNegotiation201::ChargingLimitCleared {
        cleared: ClearedChargingLimit201 {
            charging_limit_source: ChargingLimitSource201::So,
            evse_id: None,
        },
        released: true,
    };
    for (record, fixture) in [
        (
            needs(EvChargingNeedsStatus201::Processing, None),
            "ev-charging-needs-processing",
        ),
        (
            needs(
                EvChargingNeedsStatus201::Rejected,
                Some(NegotiationReason201::NotEnabled),
            ),
            "ev-charging-needs-rejected",
        ),
        (
            schedule(
                EvChargingScheduleStatus201::Accepted,
                EvScheduleBasis201::WithinCsmsSchedule,
                None,
            ),
            "ev-charging-schedule-accepted",
        ),
        (
            schedule(
                EvChargingScheduleStatus201::Rejected,
                EvScheduleBasis201::ExceedsCsmsSchedule,
                Some(NegotiationReason201::ValueTooHigh),
            ),
            "ev-charging-schedule-rejected",
        ),
        (limit, "charging-limit-ack"),
        (cleared, "cleared-charging-limit-ack"),
    ] {
        assert_eq!(
            charging_negotiation::response(&record),
            wire(fixture)[2],
            "{fixture}"
        );
    }
}

fn command(resource: ResourceRef, operation: Value) -> Command<Value> {
    let mut command: Value = serde_json::from_slice(&corpus(
        "../../../crates/contracts/tests/fixtures/command-start-v1.json",
    ))
    .unwrap();
    command["resource"] = serde_json::to_value(resource).unwrap();
    command["operation"] = operation;
    serde_json::from_value(command).unwrap()
}

fn evse_one() -> ResourceRef {
    serde_json::from_value(
        json!({"bridge_id":"bridge-berlin-1","station_id":"station-7",
        "resource":{"kind":"evse","evse_id":"evse-1"},
        "native_protocol_reference":{"protocol":"ocpp201","evse_id":1}}),
    )
    .unwrap()
}

#[test]
fn the_bridges_own_tx_profiles_are_rebuilt_only_from_exact_retained_commands() {
    for (value, unit, native, limit) in [
        ("16", "ampere", ChargingScheduleRateUnit201::A, "16"),
        (
            "16500",
            "milliampere",
            ChargingScheduleRateUnit201::A,
            "16.5",
        ),
        ("7400", "watt", ChargingScheduleRateUnit201::W, "7400"),
        ("11.1", "kilowatt", ChargingScheduleRateUnit201::W, "11100"),
    ] {
        let canonical = command(
            evse_one(),
            json!({"kind":"set_charging_limit","parameters":{"value":value,"unit":unit,"phases":3}}),
        );
        let profile = charging_negotiation::csms_tx_profile(&canonical, 77).unwrap();
        assert_eq!(profile.kind, ChargingProfileKind201::Relative);
        assert_eq!(
            (profile.stack_level, profile.valid_from, profile.valid_to),
            (0, None, None)
        );
        assert_eq!(profile.schedule.charging_rate_unit, native);
        assert_eq!(profile.schedule.id, 77);
        assert_eq!(
            profile.schedule.charging_schedule_period,
            vec![period(0, limit, Some(3))]
        );
    }
    let payload = json!({"evseId":1,"chargingProfile":{"id":-9,"stackLevel":4,
        "chargingProfilePurpose":"TxProfile","chargingProfileKind":"Absolute","transactionId":"tx-121",
        "validTo":"2026-10-07T00:00:00Z",
        "chargingSchedule":[{"id":3,"startSchedule":"2026-10-06T17:00:00Z","duration":3600,
            "chargingRateUnit":"A","chargingSchedulePeriod":[{"startPeriod":0,"limit":12.5},
                {"startPeriod":600,"limit":0}]}]}});
    let native = |payload: Value| {
        command(
            evse_one(),
            json!({"kind":"ocpp","parameters":{"protocol":"ocpp201","action":"SetChargingProfile",
                "payload_schema":"urn:OCPP:Cp:2:2020:3:SetChargingProfileRequest","payload":payload}}),
        )
    };
    let profile = charging_negotiation::csms_tx_profile(&native(payload.clone()), -9).unwrap();
    assert_eq!(profile.stack_level, 4);
    assert_eq!(profile.kind, ChargingProfileKind201::Absolute);
    assert_eq!(profile.valid_to, Some(instant("2026-10-07T00:00:00Z")));
    assert_eq!(
        profile.schedule.start_schedule,
        Some(instant("2026-10-06T17:00:00Z"))
    );
    assert_eq!(
        profile.schedule.charging_schedule_period,
        vec![period(0, "12.5", None), period(600, "0", None)]
    );
    assert!(
        charging_negotiation::csms_tx_profile(&native(payload.clone()), -8).is_none(),
        "the ledger footprint and retained request must name the same profile"
    );
    let mut default = payload;
    default["chargingProfile"]["chargingProfilePurpose"] = json!("TxDefaultProfile");
    default["chargingProfile"]
        .as_object_mut()
        .unwrap()
        .remove("transactionId");
    assert!(charging_negotiation::csms_tx_profile(&native(default), -9).is_none());
    let start = command(evse_one(), json!({"kind":"start","parameters":{}}));
    assert!(charging_negotiation::csms_tx_profile(&start, 1).is_none());
}
