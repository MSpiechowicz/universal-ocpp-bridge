use serde_json::json;
use uob_application::{
    CsmsSchedules201, CsmsTxProfile201, NegotiationPolicy201, NegotiationTransaction201,
    TransactionEventKind, TransactionEventObservation, apply_transaction_event,
    charging_needs_record_201, cleared_limit_record_201, ev_schedule_record_201,
    negotiation_transaction_201,
};
use uob_contracts::*;

fn at(seconds: i64) -> UtcTimestamp {
    UtcTimestamp::new(
        time::OffsetDateTime::from_unix_timestamp(1_791_306_000 + seconds).expect("instant"),
    )
}

fn value(id: &str, value: TypedValue) -> DataPointValue {
    DataPointValue {
        point_id: PointId::new(id).unwrap(),
        value: Some(value),
        source_time: None,
        observed_at: at(0),
        quality: Quality {
            level: QualityLevel::Good,
            reason: None,
        },
        freshness: Freshness::Unknown,
        measurement: None,
    }
}

/// The OCPP 2.0.1 fixture plus an explicit EVSE 1 resource; EVSE 2 has only a connector.
fn station() -> StationSnapshot {
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
    ))
    .unwrap();
    let mut evse = snapshot.resources[0].clone();
    evse.resource.resource = Some(CanonicalResource::Evse {
        evse_id: CanonicalEvseId::new("evse-1").unwrap(),
        connector_id: None,
    });
    evse.resource.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
        evse_id: 1,
        connector_id: None,
    });
    evse.capabilities = ResourceCapabilities::default();
    snapshot.resources.push(evse);
    snapshot.current_values.push(value(
        "ocpp201/registration/status",
        TypedValue::Text("Accepted".to_owned()),
    ));
    snapshot
}

fn transaction(snapshot: &mut StationSnapshot, id: &str, event: TransactionEventKind) {
    let observation = TransactionEventObservation {
        remote_start_id: None,
        protocol: ProtocolEdition::Ocpp201,
        event,
        id_token_present: false,
        native_transaction_id: id.to_owned(),
        native_resource: NativeProtocolReference::Ocpp201 {
            evse_id: 1,
            connector_id: Some(1),
        },
        sequence_number: u32::from(event == TransactionEventKind::Ended),
        trigger_reason: "CablePluggedIn".to_owned(),
        charging_state: None,
        stopped_reason: None,
        occurred_at: at(0),
        measurements: None,
        payload_fingerprint: format!("{id}-{event:?}"),
        reservation_id: None,
        reservation_token_key: None,
    };
    apply_transaction_event(snapshot, &observation, at(0)).unwrap();
}

fn needs(evse_id: u32) -> EvChargingNeeds201 {
    EvChargingNeeds201 {
        evse_id,
        max_schedule_tuples: Some(3),
        requested_energy_transfer: EnergyTransferMode201::Dc,
        departure_time: None,
        parameters: EvChargingParameters201::Dc(DcChargingParameters201 {
            ev_max_current: 200,
            ev_max_voltage: 800,
            energy_amount: Some(45_000),
            ev_max_power: Some(150_000),
            state_of_charge: Some(35),
            ev_energy_capacity: None,
            full_soc: None,
            bulk_soc: None,
        }),
    }
}

fn schedule(unit: ChargingScheduleRateUnit201, periods: &[(i32, &str)]) -> ChargingSchedule201 {
    ChargingSchedule201 {
        id: 1,
        duration: None,
        start_schedule: None,
        charging_rate_unit: unit,
        charging_schedule_period: periods
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

fn ev(periods: &[(i32, &str)], duration: Option<i32>) -> EvChargingSchedule201 {
    let mut native = schedule(ChargingScheduleRateUnit201::A, periods);
    native.duration = duration;
    EvChargingSchedule201 {
        evse_id: 1,
        time_base: at(0),
        period_count: u32::try_from(periods.len()).unwrap(),
        charging_schedule: Some(native),
        sales_tariff_omitted: false,
    }
}

fn profile(
    stack_level: i32,
    kind: ChargingProfileKind201,
    start: Option<i64>,
    periods: &[(i32, &str)],
) -> CsmsTxProfile201 {
    let mut schedule = schedule(ChargingScheduleRateUnit201::A, periods);
    schedule.start_schedule = start.map(at);
    CsmsTxProfile201 {
        stack_level,
        kind,
        valid_from: None,
        valid_to: None,
        schedule,
    }
}

fn decide(
    ev: EvChargingSchedule201,
    csms: &CsmsSchedules201,
) -> (
    EvChargingScheduleStatus201,
    EvScheduleBasis201,
    Option<NegotiationReason201>,
) {
    let current = NegotiationTransaction201 {
        transaction_id: TransactionId::new("tx-121").unwrap(),
        native_transaction_id: "tx-121".to_owned(),
    };
    match ev_schedule_record_201(ev, Ok(&current), csms) {
        ChargingNegotiation201::EvChargingSchedule {
            status,
            basis,
            reason,
            transaction_id,
            ..
        } => {
            assert_eq!(transaction_id, Some(current.transaction_id));
            (status, basis, reason)
        }
        other => panic!("unexpected record {other:?}"),
    }
}

fn needs_answer(
    snapshot: &StationSnapshot,
    evse_id: u32,
    processing: bool,
) -> (
    EvChargingNeedsStatus201,
    Option<NegotiationReason201>,
    Option<TransactionId>,
) {
    let policy = NegotiationPolicy201 {
        charging_needs_processing: processing,
    };
    match charging_needs_record_201(snapshot, needs(evse_id), policy) {
        ChargingNegotiation201::EvChargingNeeds {
            needs: recorded,
            status,
            reason,
            transaction_id,
        } => {
            assert_eq!(
                recorded,
                needs(evse_id),
                "the native needs are retained exactly"
            );
            (status, reason, transaction_id)
        }
        other => panic!("unexpected record {other:?}"),
    }
}

#[test]
fn charging_needs_answers_follow_the_evse_its_transaction_and_operator_policy() {
    use EvChargingNeedsStatus201::{Processing, Rejected};
    use NegotiationReason201::{NotEnabled, TxNotFound, UnknownEvse};
    let mut snapshot = station();
    for evse in [0, 2, 9] {
        assert_eq!(
            needs_answer(&snapshot, evse, true),
            (Rejected, Some(UnknownEvse), None)
        );
    }
    assert_eq!(
        needs_answer(&snapshot, 1, true),
        (Rejected, Some(TxNotFound), None)
    );
    transaction(&mut snapshot, "tx-121", TransactionEventKind::Started);
    let id = negotiation_transaction_201(&snapshot, 1)
        .unwrap()
        .transaction_id;
    assert_eq!(id.as_str(), "tx-121");
    assert_eq!(
        needs_answer(&snapshot, 1, false),
        (Rejected, Some(NotEnabled), Some(id.clone())),
        "without an operator EMS the bridge never promises a schedule (K15.FR.04)"
    );
    assert_eq!(
        needs_answer(&snapshot, 1, true),
        (Processing, None, Some(id)),
        "an operator-asserted EMS answers later through SetChargingProfile (K15.FR.05)"
    );
    transaction(&mut snapshot, "tx-121", TransactionEventKind::Ended);
    assert_eq!(
        needs_answer(&snapshot, 1, true),
        (Rejected, Some(TxNotFound), None)
    );
}

#[test]
fn ev_schedules_are_checked_only_against_exactly_known_bridge_profiles() {
    use ChargingProfileKind201::{Recurring, Relative};
    use EvChargingScheduleStatus201::{Accepted, Rejected};
    use EvScheduleBasis201::*;
    let requested = ev(&[(0, "16"), (1800, "10.5"), (3600, "0")], Some(7200));
    assert_eq!(
        decide(requested.clone(), &CsmsSchedules201::Known(Vec::new())),
        (Accepted, NoCsmsSchedule, None)
    );
    assert_eq!(
        decide(requested.clone(), &CsmsSchedules201::Unverifiable),
        (
            Rejected,
            Unverifiable,
            Some(NegotiationReason201::Unspecified)
        )
    );
    // A canonical charging limit is one unbounded relative period: valid at every instant.
    let canonical = CsmsSchedules201::Known(vec![profile(0, Relative, None, &[(0, "16")])]);
    assert_eq!(
        decide(requested.clone(), &canonical),
        (Accepted, WithinCsmsSchedule, None)
    );
    assert_eq!(
        decide(ev(&[(0, "16.1")], None), &canonical),
        (
            Rejected,
            ExceedsCsmsSchedule,
            Some(NegotiationReason201::ValueTooHigh)
        )
    );
    let mut watts = profile(0, Relative, None, &[(0, "11000")]);
    watts.schedule.charging_rate_unit = ChargingScheduleRateUnit201::W;
    for unplaceable in [
        watts,
        profile(0, Recurring, Some(0), &[(0, "16")]),
        profile(0, Relative, None, &[(0, "16"), (900, "32")]),
        profile(0, ChargingProfileKind201::Absolute, None, &[(0, "16")]),
    ] {
        assert_eq!(
            decide(
                requested.clone(),
                &CsmsSchedules201::Known(vec![unplaceable])
            )
            .1,
            Unverifiable,
            "no unit conversion, recurrence or guessed relative start"
        );
    }
}

#[test]
fn absolute_stacks_validity_and_duration_decide_each_interval_exactly() {
    use ChargingProfileKind201::Absolute;
    use EvScheduleBasis201::{ExceedsCsmsSchedule, WithinCsmsSchedule};
    let base = profile(0, Absolute, Some(0), &[(0, "32")]);
    let mut cap = profile(1, Absolute, Some(1800), &[(0, "10")]);
    cap.schedule.duration = Some(1800);
    let stack = CsmsSchedules201::Known(vec![base.clone(), cap.clone()]);
    let basis = |periods: &[(i32, &str)], duration| decide(ev(periods, duration), &stack).1;
    assert_eq!(
        basis(&[(0, "32"), (1800, "10"), (3600, "32")], None),
        WithinCsmsSchedule
    );
    assert_eq!(
        basis(&[(0, "16"), (1799, "10.1")], None),
        ExceedsCsmsSchedule,
        "the higher stack level caps the overlap even below the base profile"
    );
    assert_eq!(basis(&[(0, "32.1")], Some(1)), ExceedsCsmsSchedule);
    assert_eq!(
        basis(&[(0, "10"), (3600, "50")], Some(3600)),
        WithinCsmsSchedule,
        "a duration truncates later EV periods"
    );
    let mut bounded = base;
    bounded.valid_to = Some(at(3600));
    let mut before = cap;
    before.valid_from = Some(at(2700));
    let validity = CsmsSchedules201::Known(vec![bounded, before]);
    assert_eq!(
        decide(
            ev(&[(0, "32"), (1800, "16"), (2700, "10"), (3600, "80")], None),
            &validity
        )
        .1,
        WithinCsmsSchedule,
        "outside validity windows no bridge limit applies"
    );
    let zero = CsmsSchedules201::Known(vec![profile(0, Absolute, Some(0), &[(0, "0")])]);
    assert_eq!(decide(ev(&[(0, "0")], None), &zero).1, WithinCsmsSchedule);
    assert_eq!(
        decide(ev(&[(0, "0.1")], None), &zero).1,
        ExceedsCsmsSchedule
    );
    assert_eq!(
        decide(ev(&[(-60, "0.1"), (0, "0")], None), &zero).1,
        WithinCsmsSchedule,
        "an EV period ending before the profile starts is unconstrained"
    );
}

#[test]
fn ev_schedules_without_a_configured_evse_or_transaction_are_rejected_with_their_reason() {
    let snapshot = station();
    for (evse, basis, reason) in [
        (
            2,
            EvScheduleBasis201::UnknownEvse,
            NegotiationReason201::UnknownEvse,
        ),
        (
            1,
            EvScheduleBasis201::NoTransaction,
            NegotiationReason201::TxNotFound,
        ),
    ] {
        let mut schedule = ev(&[(0, "16")], None);
        schedule.evse_id = evse;
        let transaction = negotiation_transaction_201(&snapshot, evse);
        let record = ev_schedule_record_201(
            schedule,
            transaction.as_ref().map_err(|reason| *reason),
            &CsmsSchedules201::Known(Vec::new()),
        );
        let encoded = serde_json::to_value(&record).unwrap();
        assert_eq!(encoded["status"], "Rejected");
        assert_eq!(encoded["basis"], basis.as_str());
        assert_eq!(encoded["reason"], reason.as_str());
        assert!(encoded.get("transaction_id").is_none());
    }
}

#[test]
fn cleared_limits_report_whether_a_matching_limit_was_active() {
    let mut snapshot = station();
    snapshot.current_values.push(value(
        "ocpp201/charging-limit/SO/active",
        TypedValue::Boolean(true),
    ));
    snapshot.resources[3].current_values.push(value(
        "ocpp201/evse-1/charging-limit/EMS/active",
        TypedValue::Boolean(false),
    ));
    let released = |source, evse_id| match cleared_limit_record_201(
        &snapshot,
        ClearedChargingLimit201 {
            charging_limit_source: source,
            evse_id,
        },
    ) {
        ChargingNegotiation201::ChargingLimitCleared { released, cleared } => {
            assert_eq!(
                cleared.evse_id, evse_id,
                "the native EVSE stays as reported"
            );
            released
        }
        other => panic!("unexpected record {other:?}"),
    };
    assert!(released(ChargingLimitSource201::So, None));
    assert!(
        released(ChargingLimitSource201::So, Some(0)),
        "zero is the grid connection"
    );
    assert!(!released(ChargingLimitSource201::Ems, None));
    assert!(
        !released(ChargingLimitSource201::Ems, Some(1)),
        "already released"
    );
    assert!(!released(ChargingLimitSource201::So, Some(1)));
    assert!(!released(ChargingLimitSource201::Other, Some(9)));
    let encoded = serde_json::to_value(cleared_limit_record_201(
        &snapshot,
        ClearedChargingLimit201 {
            charging_limit_source: ChargingLimitSource201::So,
            evse_id: None,
        },
    ))
    .unwrap();
    assert_eq!(
        encoded,
        json!({"kind":"charging_limit_cleared","cleared":{"charging_limit_source":"SO"},"released":true})
    );
}
