use super::*;

#[tokio::test]
async fn canonical_only_capacity_remains_bounded_and_explicit_clear_recovers_after_pruning() {
    let database = Database::new();
    let store = database.open();
    for id in 0..128 {
        let command = reserve(
            &store,
            &format!("canonical-{id}"),
            set(footprint(
                id,
                1,
                ChargingProfilePurpose201::TxDefaultProfile,
                id,
            )),
            false,
        )
        .await
        .unwrap();
        accepted(&store, &command).await;
    }
    assert_eq!(
        store
            .charging_profile_ownership(station())
            .await
            .unwrap()
            .baseline,
        [false; 3]
    );
    assert!(
        reserve(
            &store,
            "over-capacity",
            set(footprint(
                128,
                1,
                ChargingProfilePurpose201::TxDefaultProfile,
                128
            )),
            false
        )
        .await
        .is_err()
    );
    store
        .prune_command_deduplication(at("2099-01-01T00:00:00Z"))
        .await
        .unwrap();
    assert_eq!(
        store
            .charging_profile_ownership(station())
            .await
            .unwrap()
            .footprints
            .len(),
        128
    );
    let mutation = clear(ChargingProfilePurpose201::TxDefaultProfile);
    let command = reserve(&store, "capacity-clear", mutation.clone(), false)
        .await
        .unwrap();
    clear_status(
        &store,
        &command,
        &mutation,
        ClearChargingProfileStatus201::Accepted,
    )
    .await;
    assert!(
        store
            .charging_profile_ownership(station())
            .await
            .unwrap()
            .footprints
            .is_empty()
    );
    let command = reserve(
        &store,
        "recovered-capacity",
        set(footprint(
            128,
            1,
            ChargingProfilePurpose201::TxDefaultProfile,
            128,
        )),
        false,
    )
    .await
    .unwrap();
    accepted(&store, &command).await;
    close(&store).await;
}

#[tokio::test]
async fn restart_releases_only_unsent_candidates_and_never_replays_or_accepts_late_outcomes() {
    let database = Database::new();
    let store = database.open();
    let unsent = reserve(
        &store,
        "unsent",
        set(footprint(
            1,
            1,
            ChargingProfilePurpose201::TxDefaultProfile,
            0,
        )),
        false,
    )
    .await
    .unwrap();
    close(&store).await;
    let store = database.open();
    store.interrupt_charging_profile_mutations().await.unwrap();
    assert!(
        store
            .charging_profile_ownership(station())
            .await
            .unwrap()
            .footprints
            .is_empty()
    );
    assert!(matches!(
        store
            .command_result_by_request_id(unsent.request_id)
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        CommandLifecycle::Rejected { .. }
    ));
    baseline(&store).await;
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(active_transaction_snapshot());
    store.write_atomic(write).await.unwrap();
    let mut previous = footprint(2, 1, ChargingProfilePurpose201::TxProfile, 0);
    previous.transaction_id = Some("native-tx".to_owned());
    previous.valid_from = Some(at("2026-09-01T00:00:00Z"));
    previous.valid_to = Some(at("2026-09-01T12:00:00Z"));
    let initial = reserve(&store, "previous", set(previous.clone()), true)
        .await
        .unwrap();
    accepted(&store, &initial).await;
    let mut candidate = previous.clone();
    candidate.stack_level = 1;
    candidate.valid_from = Some(at("2026-09-01T06:00:00Z"));
    candidate.valid_to = Some(at("2026-09-01T18:00:00Z"));
    let started = reserve(
        &store,
        "started",
        ProfileMutation201::Set {
            footprint: candidate.clone(),
            full_native: true,
        },
        true,
    )
    .await
    .unwrap();
    persist(&store, result(&started, CommandLifecycle::Dispatched)).await;
    close(&store).await;
    let store = database.open();
    store.interrupt_charging_profile_mutations().await.unwrap();
    let uncertain = store
        .command_result_by_request_id(started.request_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        uncertain.lifecycle,
        CommandLifecycle::TransmissionUncertain { .. }
    ));
    let ownership = store.charging_profile_ownership(station()).await.unwrap();
    assert_eq!(ownership.baseline, [true; 3]);
    assert_eq!(ownership.footprints, vec![previous, candidate]);
    assert!(!ownership.busy);
    accepted(&store, &started).await;
    assert_eq!(
        store
            .command_result_by_request_id(started.request_id)
            .await
            .unwrap(),
        Some(uncertain)
    );
    close(&store).await;
}

#[tokio::test]
async fn actual_transaction_end_atomically_retires_even_an_in_flight_candidate() {
    let database = Database::new();
    let store = database.open();
    let mut owned = footprint(1, 1, ChargingProfilePurpose201::TxProfile, 0);
    owned.transaction_id = Some("native-tx".to_owned());
    assert!(
        reserve(&store, "missing-committed-tx", set(owned.clone()), false)
            .await
            .is_err()
    );
    let mut snapshot = active_transaction_snapshot();
    let mut ambiguous = snapshot.clone();
    let mut other = ambiguous.transactions[0].clone();
    other.transaction_id = TransactionId::new("other-canonical-tx").unwrap();
    other.protocol_state.as_mut().unwrap().native_transaction_id = "other-native-tx".to_owned();
    ambiguous.transactions.push(other);
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(ambiguous);
    store.write_atomic(write).await.unwrap();
    assert!(
        reserve(
            &store,
            "ambiguous-committed-evse",
            set(owned.clone()),
            false
        )
        .await
        .is_err()
    );
    let mut uncertain = snapshot.clone();
    uncertain.transactions[0].state = TransactionState::Uncertain;
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(uncertain);
    store.write_atomic(write).await.unwrap();
    assert!(
        reserve(&store, "uncertain-committed-tx", set(owned.clone()), false)
            .await
            .is_err()
    );
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(snapshot.clone());
    store.write_atomic(write).await.unwrap();
    let command = reserve(&store, "tx-profile", set(owned.clone()), false)
        .await
        .unwrap();
    persist(&store, result(&command, CommandLifecycle::Dispatched)).await;
    let mut disconnected = snapshot.clone();
    disconnected.connectivity = Connectivity::Disconnected;
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(disconnected);
    store.write_atomic(write).await.unwrap();
    assert_eq!(
        store
            .charging_profile_ownership(station())
            .await
            .unwrap()
            .footprints
            .len(),
        1
    );
    snapshot.transactions[0].state = TransactionState::Ended;
    snapshot.transactions[0].ended_at = Some(snapshot.observed_at);
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(snapshot);
    store.write_atomic(write).await.unwrap();
    accepted(&store, &command).await;
    assert!(
        store
            .charging_profile_ownership(station())
            .await
            .unwrap()
            .footprints
            .is_empty()
    );
    assert!(
        reserve(&store, "after-end", set(owned), false)
            .await
            .is_err()
    );
    close(&store).await;
}

fn active_transaction_snapshot() -> StationSnapshot {
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
    ))
    .unwrap();
    snapshot.station = station();
    for entry in &mut snapshot.resources {
        entry.resource.bridge_id = snapshot.station.bridge_id.clone();
        entry.resource.station_id = snapshot.station.station_id.clone();
    }
    snapshot.transactions = vec![TransactionSnapshot {
        ocpp16: None,
        transaction_id: TransactionId::new("canonical-tx").unwrap(),
        resource: snapshot.resources[0].resource.clone(),
        state: TransactionState::Active,
        started_at: snapshot.observed_at,
        ended_at: None,
        protocol_state: Some(TransactionProtocolState {
            remote_start_id: None,
            protocol: ProtocolEdition::Ocpp201,
            native_transaction_id: "native-tx".to_owned(),
            native_resource: snapshot.resources[0]
                .resource
                .native_protocol_reference
                .unwrap(),
            last_sequence_number: 0,
            last_event: "Started".to_owned(),
            last_trigger_reason: "Authorized".to_owned(),
            last_event_at: snapshot.observed_at,
            last_event_fingerprint: "a".repeat(64),
        }),
    }];
    snapshot
}
