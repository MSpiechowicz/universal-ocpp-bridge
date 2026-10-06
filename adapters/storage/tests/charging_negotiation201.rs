use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use uob_application::*;
use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;

type Store = SqliteOperationalStore<Value, StationEvent, TransactionSnapshot, String>;

struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-negotiation201-{}.db", uuid::Uuid::new_v4())))
    }
    fn open(&self) -> Store {
        Store::open(&self.0, 32).unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

fn at(seconds: i64) -> UtcTimestamp {
    UtcTimestamp::new(
        time::OffsetDateTime::from_unix_timestamp(1_791_306_000 + seconds).expect("instant"),
    )
}

/// The 2.0.1 fixture with an explicit EVSE 1 resource and current accepted registration.
fn station() -> StationSnapshot {
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
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
    snapshot.current_values.push(DataPointValue {
        point_id: PointId::new("ocpp201/registration/status").unwrap(),
        value: Some(TypedValue::Text("Accepted".to_owned())),
        source_time: None,
        observed_at: at(0),
        quality: Quality {
            level: QualityLevel::Good,
            reason: None,
        },
        freshness: Freshness::Unknown,
        measurement: None,
    });
    snapshot
}

fn context(sequence: u64) -> transaction16::TransactionContext {
    transaction16::TransactionContext {
        identity: serde_json::from_value(json!({
            "bridge_id":"bridge-berlin-1",
            "runtime":{"environment":"demo","release_id":"test","release_digest":"sha256:test",
                "process_instance_id":"test"}
        }))
        .unwrap(),
        event_id: EventId::new(format!("negotiation-{sequence}")).unwrap(),
        sequence,
        correlation_id: Some(CorrelationId::new(format!("call-{sequence}")).unwrap()),
        target: None,
        delivery_deadline: at(86_400),
    }
}

async fn seeded(store: &Store) -> StationSnapshot {
    let snapshot = station();
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(snapshot.clone());
    store.write_atomic(write).await.unwrap();
    snapshot
}

async fn events(store: &Store, resource: ResourceRef) -> Vec<EventEnvelope<StationEvent>> {
    store
        .read_retained_events(RetainedEventQuery {
            resource,
            after: None,
            limit: PageLimit::new(50).unwrap(),
        })
        .await
        .unwrap()
        .events
}

fn point<'a>(values: &'a [DataPointValue], id: &str) -> Option<&'a TypedValue> {
    values
        .iter()
        .find(|value| value.point_id.as_str() == id)
        .and_then(|value| value.value.as_ref())
}

fn needs(parameters: EvChargingParameters201) -> ChargingNegotiation201 {
    ChargingNegotiation201::EvChargingNeeds {
        needs: EvChargingNeeds201 {
            evse_id: 1,
            max_schedule_tuples: Some(3),
            requested_energy_transfer: match parameters {
                EvChargingParameters201::Ac(_) => EnergyTransferMode201::AcThreePhase,
                EvChargingParameters201::Dc(_) => EnergyTransferMode201::Dc,
            },
            departure_time: Some(at(5400)),
            parameters,
        },
        status: EvChargingNeedsStatus201::Processing,
        reason: None,
        transaction_id: Some(TransactionId::new("tx-121").unwrap()),
    }
}

fn schedule(id: i32, periods: usize) -> ChargingSchedule201 {
    ChargingSchedule201 {
        id,
        duration: None,
        start_schedule: Some(at(0)),
        charging_rate_unit: ChargingScheduleRateUnit201::W,
        charging_schedule_period: (0..periods)
            .map(|index| ChargingSchedulePeriod201 {
                start_period: i32::try_from(index * 60).unwrap(),
                limit: "11000.5".parse().unwrap(),
                number_phases: Some(3),
                phase_to_use: None,
            })
            .collect(),
        min_charging_rate: None,
    }
}

fn limit(evse_id: Option<u32>, schedules: Vec<ChargingSchedule201>) -> ChargingNegotiation201 {
    ChargingNegotiation201::ChargingLimit {
        limit: ExternalChargingLimit201 {
            evse_id,
            charging_limit_source: ChargingLimitSource201::So,
            is_grid_critical: Some(true),
            charging_schedule: schedules,
            schedules_omitted: None,
            sales_tariff_omitted: false,
        },
    }
}

#[tokio::test]
async fn needs_commit_exact_evse_points_and_a_journal_record_that_survive_reopen() {
    let database = Database::new();
    let store = database.open();
    let mut snapshot = seeded(&store).await;
    let evse = snapshot.resources[3].resource.clone();
    let ac = needs(EvChargingParameters201::Ac(AcChargingParameters201 {
        energy_amount: 22_000,
        ev_min_current: 6,
        ev_max_current: 32,
        ev_max_voltage: 400,
    }));
    record_charging_negotiation_201(&store, &mut snapshot, ac.clone(), context(1), at(10))
        .await
        .unwrap();
    let values = &snapshot.resources[3].current_values;
    let prefix = "ocpp201/evse-1/ev-charging-needs/";
    for (name, expected) in [
        ("status", TypedValue::Text("Processing".to_owned())),
        ("transaction_id", TypedValue::Text("tx-121".to_owned())),
        (
            "requested_energy_transfer",
            TypedValue::Text("AC_three_phase".to_owned()),
        ),
        (
            "departure_time",
            TypedValue::Text("2026-10-06T18:30:00Z".to_owned()),
        ),
        ("energy_amount_wh", TypedValue::UnsignedInteger(22_000)),
        ("ev_min_current_a", TypedValue::UnsignedInteger(6)),
        ("ev_max_voltage_v", TypedValue::UnsignedInteger(400)),
    ] {
        assert_eq!(
            point(values, &format!("{prefix}{name}")),
            Some(&expected),
            "{name}"
        );
    }
    let [event] = events(&store, evse.clone()).await.try_into().unwrap();
    assert_eq!(event.event_type.as_str(), "station.ev_charging_needs.201");
    assert_eq!(
        (event.origin, event.source_time),
        (EventOrigin::Station, None)
    );
    assert_eq!(
        event.payload,
        StationEvent::ChargingNegotiation201 {
            station_snapshot_invalidated: snapshot.station.station_id.clone(),
            charging_negotiation_201: ac,
        }
    );
    let dc = needs(EvChargingParameters201::Dc(DcChargingParameters201 {
        ev_max_current: 200,
        ev_max_voltage: 800,
        energy_amount: None,
        ev_max_power: Some(150_000),
        state_of_charge: Some(35),
        ev_energy_capacity: None,
        full_soc: None,
        bulk_soc: Some(80),
    }));
    record_charging_negotiation_201(&store, &mut snapshot, dc, context(2), at(20))
        .await
        .unwrap();
    let values = &snapshot.resources[3].current_values;
    assert_eq!(
        point(values, &format!("{prefix}ev_min_current_a")),
        None,
        "no stale AC value"
    );
    assert_eq!(point(values, &format!("{prefix}energy_amount_wh")), None);
    assert_eq!(
        point(values, &format!("{prefix}state_of_charge_percent")),
        Some(&TypedValue::UnsignedInteger(35))
    );
    assert_eq!(snapshot.observed_at, at(20));
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    assert_eq!(
        reopened
            .station_snapshot(snapshot.station.clone())
            .await
            .unwrap(),
        Some(snapshot)
    );
    assert_eq!(events(&reopened, evse).await.len(), 2);
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn limits_are_scoped_and_refused_for_unknown_evses_without_partial_state() {
    let database = Database::new();
    let store = database.open();
    let mut snapshot = seeded(&store).await;
    let grid = limit(None, vec![schedule(-121, 3)]);
    record_charging_negotiation_201(&store, &mut snapshot, grid.clone(), context(1), at(10))
        .await
        .unwrap();
    let station_points = |snapshot: &StationSnapshot, field: &str| {
        point(
            &snapshot.current_values,
            &format!("ocpp201/charging-limit/SO/{field}"),
        )
        .cloned()
    };
    assert_eq!(
        station_points(&snapshot, "active"),
        Some(TypedValue::Boolean(true))
    );
    assert_eq!(
        station_points(&snapshot, "grid_critical"),
        Some(TypedValue::Boolean(true))
    );
    assert_eq!(
        station_points(&snapshot, "schedules"),
        Some(TypedValue::UnsignedInteger(1))
    );
    let journal = events(&store, snapshot.station.clone()).await;
    assert_eq!(journal[0].event_type.as_str(), "station.charging_limit.201");
    assert!(
        matches!(&journal[0].payload, StationEvent::ChargingNegotiation201 {
        charging_negotiation_201, ..} if *charging_negotiation_201 == grid)
    );

    let unchanged = snapshot.clone();
    let unknown = limit(Some(2), Vec::new());
    assert!(matches!(
        record_charging_negotiation_201(&store, &mut snapshot, unknown, context(2), at(20)).await,
        Err(ObservationCommitError::InvalidState)
    ));
    assert_eq!(snapshot, unchanged);
    assert_eq!(
        store
            .station_snapshot(snapshot.station.clone())
            .await
            .unwrap(),
        Some(unchanged)
    );

    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

fn grid_point(snapshot: &StationSnapshot, field: &str) -> Option<TypedValue> {
    point(
        &snapshot.current_values,
        &format!("ocpp201/charging-limit/SO/{field}"),
    )
    .cloned()
}

#[tokio::test]
async fn oversized_limits_keep_counts_and_releases_are_scoped_to_their_source_and_evse() {
    let database = Database::new();
    let store = database.open();
    let mut snapshot = seeded(&store).await;
    record_charging_negotiation_201(
        &store,
        &mut snapshot,
        limit(None, Vec::new()),
        context(1),
        at(10),
    )
    .await
    .unwrap();
    let large = limit(Some(1), (0..4).map(|id| schedule(id, 1024)).collect());
    record_charging_negotiation_201(&store, &mut snapshot, large, context(2), at(30))
        .await
        .unwrap();
    let evse = snapshot.resources[3].resource.clone();
    let [event] = events(&store, evse).await.try_into().unwrap();
    let StationEvent::ChargingNegotiation201 {
        charging_negotiation_201: ChargingNegotiation201::ChargingLimit { limit },
        ..
    } = &event.payload
    else {
        panic!("limit evidence");
    };
    assert!(limit.charging_schedule.is_empty());
    assert_eq!(
        limit.schedules_omitted,
        Some(4),
        "omitted schedules stay counted"
    );
    assert!(
        serde_json::to_vec(&event.payload).unwrap().len()
            <= CHARGING_NEGOTIATION_EVIDENCE_LIMIT_201
    );
    assert_eq!(
        point(
            &snapshot.resources[3].current_values,
            "ocpp201/evse-1/charging-limit/SO/schedules"
        ),
        Some(&TypedValue::UnsignedInteger(4))
    );

    let cleared = cleared_limit_record_201(
        &snapshot,
        ClearedChargingLimit201 {
            charging_limit_source: ChargingLimitSource201::So,
            evse_id: Some(0),
        },
    );
    assert!(matches!(
        cleared,
        ChargingNegotiation201::ChargingLimitCleared { released: true, .. }
    ));
    record_charging_negotiation_201(&store, &mut snapshot, cleared, context(3), at(40))
        .await
        .unwrap();
    assert_eq!(
        grid_point(&snapshot, "active"),
        Some(TypedValue::Boolean(false))
    );
    assert_eq!(grid_point(&snapshot, "grid_critical"), None);
    assert_eq!(grid_point(&snapshot, "schedules"), None);
    assert_eq!(
        point(
            &snapshot.resources[3].current_values,
            "ocpp201/evse-1/charging-limit/SO/active"
        ),
        Some(&TypedValue::Boolean(true)),
        "a grid release does not release an EVSE-scoped limit"
    );
    assert_eq!(events(&store, snapshot.station.clone()).await.len(), 2);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn unregistered_stations_and_invalid_contexts_commit_nothing() {
    let database = Database::new();
    let store = database.open();
    let mut snapshot = seeded(&store).await;
    let record = limit(None, Vec::new());
    for context in [
        context(0),
        transaction16::TransactionContext {
            identity: serde_json::from_value(json!({
                "bridge_id":"another-bridge",
                "runtime":{"environment":"demo","release_id":"test","release_digest":"sha256:test",
                    "process_instance_id":"test"}
            }))
            .unwrap(),
            ..context(1)
        },
    ] {
        assert!(matches!(
            record_charging_negotiation_201(&store, &mut snapshot, record.clone(), context, at(10))
                .await,
            Err(ObservationCommitError::InvalidState)
        ));
    }
    snapshot.current_values.clear();
    let unregistered = snapshot.clone();
    assert!(matches!(
        record_charging_negotiation_201(&store, &mut snapshot, record, context(2), at(10)).await,
        Err(ObservationCommitError::InvalidState)
    ));
    assert_eq!(snapshot, unregistered);
    assert!(events(&store, snapshot.station.clone()).await.is_empty());
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
