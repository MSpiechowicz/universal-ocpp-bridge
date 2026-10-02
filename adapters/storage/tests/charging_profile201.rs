#[path = "charging_profile201/authority_and_ids.rs"]
mod authority_and_ids;
#[path = "charging_profile201/fixtures.rs"]
mod fixtures;
#[path = "charging_profile201/recovery.rs"]
mod recovery;
use fixtures::*;
use uob_application::*;
use uob_contracts::*;

#[tokio::test]
async fn explicit_partial_baseline_survives_reopen_and_targeted_unknown_does_not_establish_it() {
    let database = Database::new();
    let store = database.open();
    let candidate = set(footprint(
        1,
        1,
        ChargingProfilePurpose201::TxDefaultProfile,
        0,
    ));
    assert_eq!(
        reserve(&store, "blocked", candidate.clone(), true)
            .await
            .unwrap_err()
            .code(),
        StorageErrorCode::Conflict
    );
    assert!(
        store
            .command_by_request_id(RequestId::new("blocked").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    let mutation = clear(ChargingProfilePurpose201::ChargingStationMaxProfile);
    let command = reserve(&store, "partial", mutation.clone(), false)
        .await
        .unwrap();
    clear_status(
        &store,
        &command,
        &mutation,
        ClearChargingProfileStatus201::Unknown,
    )
    .await;
    let targeted = ProfileMutation201::Clear(ClearChargingProfileRequest201 {
        charging_profile_id: Some(7),
        charging_profile_criteria: None,
    });
    let command = reserve(&store, "targeted", targeted.clone(), false)
        .await
        .unwrap();
    clear_status(
        &store,
        &command,
        &targeted,
        ClearChargingProfileStatus201::Unknown,
    )
    .await;
    close(&store).await;
    let reopened = database.open();
    assert_eq!(
        reopened
            .charging_profile_ownership(station())
            .await
            .unwrap()
            .baseline,
        [true, false, false]
    );
    assert!(
        reserve(&reopened, "still-blocked", candidate.clone(), true)
            .await
            .is_err()
    );
    for (id, purpose) in [
        ("default", ChargingProfilePurpose201::TxDefaultProfile),
        ("tx", ChargingProfilePurpose201::TxProfile),
    ] {
        let mutation = clear(purpose);
        let command = reserve(&reopened, id, mutation.clone(), false)
            .await
            .unwrap();
        clear_status(
            &reopened,
            &command,
            &mutation,
            ClearChargingProfileStatus201::Accepted,
        )
        .await;
    }
    let command = reserve(&reopened, "ready", candidate, true).await.unwrap();
    accepted(&reopened, &command).await;
    assert_eq!(
        reopened
            .charging_profile_ownership(station())
            .await
            .unwrap()
            .footprints[0]
            .id,
        1
    );
    close(&reopened).await;
}

#[tokio::test]
async fn uncertain_native_replacements_keep_both_scopes_until_explicit_clear() {
    let database = Database::new();
    let store = database.open();
    let other = database.open();
    baseline(&store).await;
    let owned = uncertain_replacement(&store, &other).await;
    assert!(
        reserve(
            &store,
            "cross-evse-id",
            set(footprint(
                1,
                2,
                ChargingProfilePurpose201::TxDefaultProfile,
                3
            )),
            true
        )
        .await
        .is_err()
    );
    assert_eq!(
        store
            .charging_profile_ownership(station())
            .await
            .unwrap()
            .footprints,
        owned.footprints
    );
    let targeted = ProfileMutation201::Clear(ClearChargingProfileRequest201 {
        charging_profile_id: Some(1),
        charging_profile_criteria: None,
    });
    let clear_command = reserve(&store, "uncertain-no-match", targeted.clone(), false)
        .await
        .unwrap();
    clear_status(
        &store,
        &clear_command,
        &targeted,
        ClearChargingProfileStatus201::Unknown,
    )
    .await;
    assert_eq!(
        store
            .charging_profile_ownership(station())
            .await
            .unwrap()
            .footprints
            .len(),
        2
    );
    let clear_command = reserve(&store, "confirmed-clear", targeted.clone(), false)
        .await
        .unwrap();
    clear_status(
        &store,
        &clear_command,
        &targeted,
        ClearChargingProfileStatus201::Accepted,
    )
    .await;
    let cross = reserve(
        &store,
        "cross-after-clear",
        set(footprint(
            1,
            2,
            ChargingProfilePurpose201::TxDefaultProfile,
            3,
        )),
        true,
    )
    .await
    .unwrap();
    accepted(&store, &cross).await;
    assert_eq!(
        store
            .charging_profile_ownership(station())
            .await
            .unwrap()
            .footprints[0]
            .evse_id,
        2
    );
    close(&store).await;
    close(&other).await;
}

async fn uncertain_replacement(store: &Store, other: &Store) -> ProfileOwnership201 {
    let initial = set(footprint(
        1,
        1,
        ChargingProfilePurpose201::TxDefaultProfile,
        0,
    ));
    let command = reserve(store, "initial", initial, true).await.unwrap();
    accepted(store, &command).await;
    let replacement = ProfileMutation201::Set {
        footprint: footprint(1, 1, ChargingProfilePurpose201::TxDefaultProfile, 2),
        full_native: true,
    };
    let command = reserve(other, "replacement", replacement, true)
        .await
        .unwrap();
    persist(other, result(&command, CommandLifecycle::Dispatched)).await;
    persist(
        other,
        result(
            &command,
            CommandLifecycle::TransmissionUncertain {
                detail: "socket lost".to_owned(),
            },
        ),
    )
    .await;
    let owned = store.charging_profile_ownership(station()).await.unwrap();
    assert_eq!(owned.footprints.len(), 2);
    assert!(
        owned
            .footprints
            .iter()
            .any(|footprint| footprint.stack_level == 0)
    );
    assert!(
        owned
            .footprints
            .iter()
            .any(|footprint| footprint.stack_level == 2)
    );
    assert!(!owned.busy);
    owned
}

#[tokio::test]
async fn concurrent_writers_enforce_station_singleflight_before_terminal_acknowledgement() {
    let database = Database::new();
    let store = database.open();
    let other = database.open();
    baseline(&store).await;
    let initial = footprint(1, 1, ChargingProfilePurpose201::TxDefaultProfile, 0);
    let command = reserve(&store, "initial", set(initial.clone()), true)
        .await
        .unwrap();
    assert!(
        reserve(
            &other,
            "busy",
            set(footprint(
                2,
                2,
                ChargingProfilePurpose201::TxDefaultProfile,
                1
            )),
            true
        )
        .await
        .is_err()
    );
    let ownership = store.charging_profile_ownership(station()).await.unwrap();
    assert_eq!(ownership.footprints, vec![initial]);
    assert!(ownership.busy);
    accepted(&store, &command).await;
    assert!(
        !store
            .charging_profile_ownership(station())
            .await
            .unwrap()
            .busy
    );
    close(&store).await;
    close(&other).await;
}

#[tokio::test]
async fn touching_windows_do_not_conflict_but_overlap_cross_scope_defaults_and_fr39_do() {
    let database = Database::new();
    let store = database.open();
    baseline(&store).await;
    let mut first = footprint(1, 1, ChargingProfilePurpose201::TxDefaultProfile, 0);
    first.valid_to = Some(at("2026-09-01T12:00:00Z"));
    let command = reserve(&store, "first", set(first.clone()), true)
        .await
        .unwrap();
    accepted(&store, &command).await;
    let mut touching = footprint(2, 1, ChargingProfilePurpose201::TxDefaultProfile, 0);
    touching.valid_from = first.valid_to;
    let command = reserve(&store, "touching", set(touching), true)
        .await
        .unwrap();
    accepted(&store, &command).await;
    assert!(
        reserve(
            &store,
            "overlap",
            set(footprint(
                3,
                1,
                ChargingProfilePurpose201::TxDefaultProfile,
                0
            )),
            true
        )
        .await
        .is_err()
    );
    assert!(
        reserve(
            &store,
            "station-default",
            set(footprint(
                4,
                0,
                ChargingProfilePurpose201::TxDefaultProfile,
                0
            )),
            true
        )
        .await
        .is_err()
    );
    let mut a = footprint(1, 1, ChargingProfilePurpose201::TxProfile, 0);
    a.transaction_id = Some("native-TX".to_owned());
    a.valid_to = Some(at("2026-09-01T12:00:00Z"));
    let mut b = a.clone();
    b.id = 2;
    b.valid_from = a.valid_to;
    b.valid_to = None;
    assert!(a.conflicts(&b), "FR39 has no overlap exception");
    b.transaction_id = Some("other-TX".to_owned());
    assert!(!a.conflicts(&b));
    b.valid_from = None;
    assert!(
        a.conflicts(&b),
        "FR06 also protects overlapping same-EVSE TxProfile ownership"
    );
    b.evse_id = 2;
    assert!(!a.conflicts(&b));
    first.evse_id = 0;
    let mut station_later = footprint(9, 0, ChargingProfilePurpose201::TxDefaultProfile, 0);
    station_later.valid_from = first.valid_to;
    assert!(!first.conflicts(&station_later));
    station_later.evse_id = 1;
    assert!(
        first.conflicts(&station_later),
        "cross-scope TxDefault protection has no overlap exception"
    );
    close(&store).await;
}
