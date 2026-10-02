use super::*;

#[tokio::test]
async fn canonical_replacement_cannot_erase_incompatible_known_native_metadata_after_history_pruning()
 {
    let database = Database::new();
    let store = database.open();
    let protected = footprint(7, 1, ChargingProfilePurpose201::TxDefaultProfile, 3);
    let command = reserve(
        &store,
        "native-protected-policy",
        ProfileMutation201::Set {
            footprint: protected.clone(),
            full_native: true,
        },
        false,
    )
    .await
    .unwrap();
    let request = SetChargingProfileRequest201 {
        evse_id: 1,
        charging_profile: ChargingProfile201 {
            id: 7,
            stack_level: 3,
            charging_profile_purpose: ChargingProfilePurpose201::TxDefaultProfile,
            charging_profile_kind: ChargingProfileKind201::Absolute,
            transaction_id: None,
            recurrency_kind: None,
            valid_from: None,
            valid_to: None,
            charging_schedule: vec![ChargingSchedule201 {
                id: 7,
                duration: None,
                start_schedule: Some(command.admitted_at),
                charging_rate_unit: ChargingScheduleRateUnit201::A,
                min_charging_rate: None,
                charging_schedule_period: vec![ChargingSchedulePeriod201 {
                    start_period: 0,
                    limit: ExactDecimal::new(1, 0),
                    number_phases: None,
                    phase_to_use: None,
                }],
            }],
        },
    };
    let mut acknowledgement = result(
        &command,
        CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None,
        },
    );
    acknowledgement.schema_version = ContractVersion::V1_CHARGING_PROFILE_201;
    acknowledgement.charging_profile_201 = Some(ChargingProfileResult201::SetChargingProfile {
        request,
        status: SetChargingProfileStatus201::Accepted,
        reason_code: None,
    });
    persist(&store, acknowledgement).await;
    store
        .prune_command_deduplication(at("2099-01-01T00:00:00Z"))
        .await
        .unwrap();
    assert_eq!(
        store
            .command_result_by_request_id(command.request_id)
            .await
            .unwrap(),
        None
    );
    let incompatible = footprint(7, 1, ChargingProfilePurpose201::TxDefaultProfile, 0);
    let error = reserve(&store, "canonical-collision", set(incompatible), false)
        .await
        .unwrap_err();
    assert_eq!(error.code(), StorageErrorCode::Conflict);
    let ownership = store.charging_profile_ownership(station()).await.unwrap();
    assert_eq!(ownership.footprints, vec![protected]);
    assert!(!ownership.busy);
    close(&store).await;
}

#[tokio::test]
async fn long_external_identifiers_survive_dispatch_terminal_reopen_recovery_and_explicit_clear() {
    let database = Database::new();
    let store = database.open();
    let id = format!("accepted-{}", "x".repeat(12_000));
    let owned = footprint(11, 1, ChargingProfilePurpose201::TxDefaultProfile, 0);
    let command = reserve(&store, &id, set(owned.clone()), false)
        .await
        .unwrap();
    persist(&store, result(&command, CommandLifecycle::Dispatched)).await;
    accepted(&store, &command).await;
    assert_eq!(
        store
            .command_result_by_request_id(command.request_id.clone())
            .await
            .unwrap()
            .unwrap()
            .return_route
            .request_id
            .as_str(),
        id
    );
    let interrupted_id = format!("interrupted-{}", "y".repeat(12_000));
    let interrupted = reserve(&store, &interrupted_id, set(owned.clone()), false)
        .await
        .unwrap();
    persist(&store, result(&interrupted, CommandLifecycle::Dispatched)).await;
    close(&store).await;

    let store = database.open();
    store.interrupt_charging_profile_mutations().await.unwrap();
    let recovered = store
        .command_result_by_request_id(interrupted.request_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.return_route.request_id.as_str(), interrupted_id);
    assert!(matches!(
        recovered.lifecycle,
        CommandLifecycle::TransmissionUncertain { .. }
    ));
    let ownership = store.charging_profile_ownership(station()).await.unwrap();
    assert_eq!(ownership.footprints, vec![owned.clone(), owned]);
    assert!(!ownership.busy);
    let clear = ProfileMutation201::Clear(ClearChargingProfileRequest201 {
        charging_profile_id: Some(11),
        charging_profile_criteria: None,
    });
    let clear_id = format!("clear-{}", "z".repeat(12_000));
    let clearing = reserve(&store, &clear_id, clear.clone(), false)
        .await
        .unwrap();
    persist(&store, result(&clearing, CommandLifecycle::Dispatched)).await;
    clear_status(
        &store,
        &clearing,
        &clear,
        ClearChargingProfileStatus201::Accepted,
    )
    .await;
    accepted(&store, &interrupted).await;
    assert_eq!(
        store
            .command_result_by_request_id(interrupted.request_id)
            .await
            .unwrap(),
        Some(recovered)
    );
    let cleared = store.charging_profile_ownership(station()).await.unwrap();
    assert!(cleared.footprints.is_empty());
    assert!(!cleared.busy);
    assert_eq!(
        store
            .command_result_by_request_id(clearing.request_id)
            .await
            .unwrap()
            .unwrap()
            .return_route
            .request_id
            .as_str(),
        clear_id
    );
    close(&store).await;
}

#[tokio::test]
async fn long_unsent_identifiers_recover_rejection_and_release_the_reservation() {
    let database = Database::new();
    let store = database.open();
    let unsent_id = format!("unsent-{}", "u".repeat(12_000));
    let unsent = reserve(
        &store,
        &unsent_id,
        set(footprint(
            12,
            1,
            ChargingProfilePurpose201::TxDefaultProfile,
            0,
        )),
        false,
    )
    .await
    .unwrap();
    store.interrupt_charging_profile_mutations().await.unwrap();
    let unsent = store
        .command_result_by_request_id(unsent.request_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unsent.return_route.request_id.as_str(), unsent_id);
    assert!(matches!(
        unsent.lifecycle,
        CommandLifecycle::Rejected { .. }
    ));
    assert!(
        store
            .charging_profile_ownership(station())
            .await
            .unwrap()
            .footprints
            .is_empty()
    );
    close(&store).await;
}
