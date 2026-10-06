use super::fixtures::*;
use uob_application::*;
use uob_contracts::*;

fn owner(id: &str, state: ProfileOwnershipState201, profile: i32, stack: i32) -> ProfileOwner201 {
    ProfileOwner201 {
        request_id: RequestId::new(id).unwrap(),
        state,
        footprint: footprint(
            profile,
            1,
            ChargingProfilePurpose201::TxDefaultProfile,
            stack,
        ),
    }
}

#[tokio::test]
async fn owners_attribute_each_footprint_to_its_request_and_durable_native_state() {
    use ProfileOwnershipState201::{Owned, Reserved, Uncertain};
    let database = Database::new();
    let store = database.open();
    let owned = reserve(
        &store,
        "owned",
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
    assert_eq!(
        store.charging_profile_owners(station()).await.unwrap(),
        ProfileOwners201 {
            owners: vec![owner("owned", Reserved, 1, 0)],
            busy: true,
        },
        "an admitted mutation is neither owned nor finished"
    );
    accepted(&store, &owned).await;
    let lost = reserve(
        &store,
        "lost",
        set(footprint(
            2,
            1,
            ChargingProfilePurpose201::TxDefaultProfile,
            1,
        )),
        false,
    )
    .await
    .unwrap();
    persist(&store, result(&lost, CommandLifecycle::Dispatched)).await;
    persist(
        &store,
        result(
            &lost,
            CommandLifecycle::TransmissionUncertain {
                detail: "reply lost".to_owned(),
            },
        ),
    )
    .await;
    let expected = ProfileOwners201 {
        owners: vec![owner("owned", Owned, 1, 0), owner("lost", Uncertain, 2, 1)],
        busy: false,
    };
    assert_eq!(
        store.charging_profile_owners(station()).await.unwrap(),
        expected
    );
    assert_eq!(
        store
            .charging_profile_ownership(station())
            .await
            .unwrap()
            .footprints,
        expected
            .owners
            .iter()
            .map(|owner| owner.footprint.clone())
            .collect::<Vec<_>>(),
        "both views read one ledger"
    );
    close(&store).await;
    let reopened = database.open();
    assert_eq!(
        reopened.charging_profile_owners(station()).await.unwrap(),
        expected
    );
    let mut evse = station();
    evse.resource = Some(CanonicalResource::Evse {
        evse_id: CanonicalEvseId::new("evse-1").unwrap(),
        connector_id: None,
    });
    assert_eq!(
        reopened
            .charging_profile_owners(evse)
            .await
            .unwrap_err()
            .code(),
        StorageErrorCode::InvalidRequest,
        "the ledger is station scoped"
    );
    close(&reopened).await;
}
