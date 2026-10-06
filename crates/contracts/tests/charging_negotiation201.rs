use serde::Deserialize;
use serde_json::{Value, json};
use uob_contracts::*;

fn station() -> StationId {
    StationId::new("station-121").unwrap()
}

fn schedule(id: i32, limits: &[(i32, &str)]) -> ChargingSchedule201 {
    ChargingSchedule201 {
        id,
        duration: None,
        start_schedule: None,
        charging_rate_unit: ChargingScheduleRateUnit201::A,
        charging_schedule_period: limits
            .iter()
            .map(|(start, limit)| ChargingSchedulePeriod201 {
                start_period: *start,
                limit: limit.parse().unwrap(),
                number_phases: None,
                phase_to_use: None,
            })
            .collect(),
        min_charging_rate: None,
    }
}

fn records() -> Vec<ChargingNegotiation201> {
    let transaction = TransactionId::new("tx-121").unwrap();
    vec![
        ChargingNegotiation201::EvChargingNeeds {
            needs: EvChargingNeeds201 {
                evse_id: 1,
                max_schedule_tuples: Some(3),
                requested_energy_transfer: EnergyTransferMode201::AcThreePhase,
                departure_time: Some(
                    serde_json::from_value(json!("2026-10-06T18:30:00Z")).unwrap(),
                ),
                parameters: EvChargingParameters201::Ac(AcChargingParameters201 {
                    energy_amount: 22_000,
                    ev_min_current: 6,
                    ev_max_current: 32,
                    ev_max_voltage: 400,
                }),
            },
            status: EvChargingNeedsStatus201::Processing,
            reason: None,
            transaction_id: Some(transaction.clone()),
        },
        ChargingNegotiation201::EvChargingSchedule {
            schedule: EvChargingSchedule201 {
                evse_id: 1,
                time_base: serde_json::from_value(json!("2026-10-06T17:00:00Z")).unwrap(),
                charging_schedule: Some(schedule(7, &[(0, "16"), (1800, "0")])),
                period_count: 2,
                sales_tariff_omitted: true,
            },
            status: EvChargingScheduleStatus201::Rejected,
            basis: EvScheduleBasis201::ExceedsCsmsSchedule,
            reason: Some(NegotiationReason201::ValueTooHigh),
            transaction_id: Some(transaction),
        },
        ChargingNegotiation201::ChargingLimit {
            limit: ExternalChargingLimit201 {
                evse_id: None,
                charging_limit_source: ChargingLimitSource201::So,
                is_grid_critical: Some(true),
                charging_schedule: vec![schedule(-121, &[(0, "11000"), (900, "7400.5")])],
                schedules_omitted: None,
                sales_tariff_omitted: false,
            },
        },
        ChargingNegotiation201::ChargingLimitCleared {
            cleared: ClearedChargingLimit201 {
                charging_limit_source: ChargingLimitSource201::Ems,
                evse_id: Some(0),
            },
            released: false,
        },
    ]
}

#[test]
fn negotiation_records_round_trip_as_distinct_station_events() {
    for record in records() {
        let event = StationEvent::ChargingNegotiation201 {
            station_snapshot_invalidated: station(),
            charging_negotiation_201: record.clone(),
        };
        let encoded = serde_json::to_value(&event).unwrap();
        assert_eq!(encoded["station_snapshot_invalidated"], "station-121");
        assert!(encoded["charging_negotiation_201"]["kind"].is_string());
        assert_eq!(
            serde_json::from_value::<StationEvent>(encoded).unwrap(),
            event
        );
    }
    let invalidation = StationEvent::Invalidation {
        station_snapshot_invalidated: station(),
    };
    assert_eq!(
        serde_json::from_value::<StationEvent>(serde_json::to_value(&invalidation).unwrap())
            .unwrap(),
        invalidation,
        "a plain marker never decodes as negotiation evidence"
    );
}

/// The station journal variants released before this contract addition.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
enum ReleasedStationEvent {
    StationSnapshot(StationSnapshot),
    Transaction(TransactionSnapshot),
    TriggerNotification {
        station_snapshot_invalidated: StationId,
        class: TriggerMessageClass,
        connector_id: Option<u32>,
        status: Option<String>,
    },
    TriggerNotification201 {
        station_snapshot_invalidated: StationId,
        trigger_class_201: TriggerMessageClass201,
        target: TriggerTarget201,
        status: Option<String>,
    },
    Invalidation {
        station_snapshot_invalidated: StationId,
    },
}

#[test]
fn released_readers_decode_negotiation_evidence_as_an_invalidation_marker() {
    for record in records() {
        let encoded = serde_json::to_value(StationEvent::ChargingNegotiation201 {
            station_snapshot_invalidated: station(),
            charging_negotiation_201: record,
        })
        .unwrap();
        match serde_json::from_value::<ReleasedStationEvent>(encoded).unwrap() {
            ReleasedStationEvent::Invalidation {
                station_snapshot_invalidated,
            } => assert_eq!(station_snapshot_invalidated, station()),
            other => panic!("released reader misread negotiation evidence: {other:?}"),
        }
    }
}

#[test]
fn native_spellings_match_the_pinned_enumerations_and_reason_appendix() {
    for (mode, native) in [
        (EnergyTransferMode201::Dc, "DC"),
        (EnergyTransferMode201::AcSinglePhase, "AC_single_phase"),
        (EnergyTransferMode201::AcTwoPhase, "AC_two_phase"),
        (EnergyTransferMode201::AcThreePhase, "AC_three_phase"),
    ] {
        assert_eq!(mode.as_str(), native);
        assert_eq!(serde_json::to_value(mode).unwrap(), native);
    }
    for status in [
        EvChargingNeedsStatus201::Accepted,
        EvChargingNeedsStatus201::Rejected,
        EvChargingNeedsStatus201::Processing,
    ] {
        assert_eq!(serde_json::to_value(status).unwrap(), status.as_str());
    }
    for status in [
        EvChargingScheduleStatus201::Accepted,
        EvChargingScheduleStatus201::Rejected,
    ] {
        assert_eq!(serde_json::to_value(status).unwrap(), status.as_str());
    }
    for reason in [
        NegotiationReason201::UnknownEvse,
        NegotiationReason201::TxNotFound,
        NegotiationReason201::NotEnabled,
        NegotiationReason201::ValueTooHigh,
        NegotiationReason201::Unspecified,
    ] {
        assert_eq!(serde_json::to_value(reason).unwrap(), reason.as_str());
        assert!(
            reason.as_str().len() <= 20,
            "StatusInfoType.reasonCode is string[0..20]"
        );
    }
    assert_eq!(
        serde_json::to_value(EvScheduleBasis201::NoCsmsSchedule).unwrap(),
        "no_csms_schedule"
    );
}

#[test]
fn omitted_evidence_stays_explicit_and_exact_values_keep_their_spelling() {
    let records = records();
    let ChargingNegotiation201::EvChargingSchedule { schedule, .. } = &records[1] else {
        unreachable!()
    };
    let mut omitted = schedule.clone();
    omitted.charging_schedule = None;
    let encoded = serde_json::to_value(&omitted).unwrap();
    assert!(encoded.get("charging_schedule").is_none());
    assert_eq!(encoded["period_count"], 2);
    assert_eq!(encoded["sales_tariff_omitted"], true);
    let limit: Value = serde_json::to_value(&records[2]).unwrap();
    assert_eq!(limit["kind"], "charging_limit");
    assert_eq!(limit["limit"]["charging_limit_source"], "SO");
    assert_eq!(
        limit["limit"]["charging_schedule"][0]["charging_schedule_period"][1]["limit"],
        "7400.5"
    );
    assert!(limit["limit"].get("schedules_omitted").is_none());
    assert!(limit["limit"].get("sales_tariff_omitted").is_none());
    let cleared: Value = serde_json::to_value(&records[3]).unwrap();
    assert_eq!(
        cleared["cleared"]["evse_id"], 0,
        "a reported zero stays zero"
    );
    const { assert!(CHARGING_NEGOTIATION_EVIDENCE_LIMIT_201 < 64 * 1024) };
}
