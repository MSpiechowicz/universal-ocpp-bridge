//! `ExtendedTriggerMessage` evidence: Whitepaper-only classes, revision 19 and class isolation.
use super::*;

const LEGACY_REVISION: u16 = 2;
const EXTENDED_REVISION: u16 = 19;

fn extended_command(action: &str) -> Command<String> {
    Command {
        operation: CommandOperation::Ocpp(PrivilegedOcppOperation {
            protocol: ProtocolEdition::Ocpp16j,
            action: ProtocolActionName::new(action).unwrap(),
            payload_schema: PayloadSchemaId::new("extended-trigger-message-16").unwrap(),
            payload: "station".to_owned(),
        }),
        ..command()
    }
}

fn station_result(
    class: TriggerMessageClass,
    response: Option<TriggerNativeResponse>,
) -> CommandResult {
    let mut result = result(response);
    result.schema_version = class.contract_version();
    let observation = result.trigger_observation.as_mut().unwrap();
    observation.requested_class = class;
    observation.expected_targets = vec![0];
    result
}

fn station_marker(
    id: &str,
    sequence: u64,
    class: TriggerMessageClass,
    observed: i64,
) -> EventEnvelope<StationEvent> {
    let mut marker = event(id, sequence, 0, class, observed);
    if let StationEvent::TriggerNotification { connector_id, .. } = &mut marker.payload {
        *connector_id = None;
    }
    marker
}

async fn admit_extended(store: &Store, action: &str, class: TriggerMessageClass) {
    let mut write = AtomicStoreWrite::<String, StationEvent, (), ()>::empty();
    write.command = Some(extended_command(action));
    write.command_result = Some(station_result(class, None));
    store.write_atomic(write).await.unwrap();
}

async fn reconcile(store: &Store, seconds: i64) -> CommandResult {
    store
        .reconcile_trigger_observation(command().request_id, at(seconds))
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn pending_extended_command_is_observed_by_a_matching_marker() {
    for class in [
        TriggerMessageClass::LogStatusNotification,
        TriggerMessageClass::SignedFirmwareStatusNotification,
    ] {
        let database = Database::new();
        let store = Store::open(&database.0, 16).unwrap();
        admit_extended(&store, "ExtendedTriggerMessage", class).await;
        assert!(
            store
                .trigger_pending_for_station(station(), class, at(3))
                .await
                .unwrap()
        );
        put_result(
            &store,
            station_result(class, Some(TriggerNativeResponse::Accepted)),
        )
        .await;
        assert_eq!(
            state(&reconcile(&store, 3).await),
            TriggerObservationStatus::Pending
        );

        put_event(&store, station_marker("marker", 1, class, 4)).await;
        let observed = reconcile(&store, 5).await;

        assert_eq!(state(&observed), TriggerObservationStatus::Observed);
        assert_eq!(
            observed
                .trigger_observation
                .as_ref()
                .unwrap()
                .observed
                .len(),
            1
        );
        assert_eq!(observed.schema_version.revision, EXTENDED_REVISION);
        let stored = store
            .command_result_by_request_id(command().request_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored, observed);
    }
}

#[tokio::test]
async fn plain_and_signed_firmware_markers_never_cross_match() {
    let pairs = [
        (
            "ExtendedTriggerMessage",
            TriggerMessageClass::SignedFirmwareStatusNotification,
            TriggerMessageClass::FirmwareStatusNotification,
        ),
        (
            "TriggerMessage",
            TriggerMessageClass::FirmwareStatusNotification,
            TriggerMessageClass::SignedFirmwareStatusNotification,
        ),
    ];
    for (action, requested, other) in pairs {
        let database = Database::new();
        let store = Store::open(&database.0, 16).unwrap();
        admit_extended(&store, action, requested).await;
        put_result(
            &store,
            station_result(requested, Some(TriggerNativeResponse::Accepted)),
        )
        .await;

        put_event(&store, station_marker("other", 1, other, 4)).await;
        let unmatched = reconcile(&store, 5).await;
        assert_eq!(
            state(&unmatched),
            TriggerObservationStatus::Pending,
            "{requested:?}"
        );
        assert!(unmatched.trigger_observation.unwrap().observed.is_empty());

        put_event(&store, station_marker("same", 2, requested, 6)).await;
        let observed = reconcile(&store, 7).await;
        assert_eq!(
            state(&observed),
            TriggerObservationStatus::Observed,
            "{requested:?}"
        );
        assert_eq!(
            observed.trigger_observation.unwrap().observed[0]
                .event_id
                .as_str(),
            "same"
        );
    }
}

#[tokio::test]
async fn result_revision_follows_the_class() {
    let cases = [
        (
            TriggerMessageClass::LogStatusNotification,
            EXTENDED_REVISION,
        ),
        (
            TriggerMessageClass::SignedFirmwareStatusNotification,
            EXTENDED_REVISION,
        ),
        (
            TriggerMessageClass::FirmwareStatusNotification,
            LEGACY_REVISION,
        ),
        (TriggerMessageClass::Heartbeat, LEGACY_REVISION),
        (TriggerMessageClass::StatusNotification, LEGACY_REVISION),
    ];
    for (class, revision) in cases {
        let database = Database::new();
        let store = Store::open(&database.0, 16).unwrap();
        admit_extended(&store, "ExtendedTriggerMessage", class).await;
        // A later write without observation keeps the class revision of the stored window.
        let mut update = station_result(class, Some(TriggerNativeResponse::Accepted));
        update.schema_version = ContractVersion::V1_INITIAL;
        put_result(&store, update).await;
        let stored = store
            .command_result_by_request_id(command().request_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.schema_version.revision, revision, "{class:?}");
    }
}

#[tokio::test]
async fn extended_action_is_accepted_and_other_actions_are_refused() {
    let class = TriggerMessageClass::LogStatusNotification;
    let database = Database::new();
    let store = Store::open(&database.0, 16).unwrap();
    admit_extended(&store, "ExtendedTriggerMessage", class).await;
    assert!(
        store
            .reconcile_trigger_observation(command().request_id, at(3))
            .await
            .is_ok()
    );

    for action in ["SetChargingProfile", "ExtendedTriggerMessageX"] {
        let database = Database::new();
        let store = Store::open(&database.0, 16).unwrap();
        admit_extended(&store, action, class).await;
        assert!(
            store
                .reconcile_trigger_observation(command().request_id, at(3))
                .await
                .is_err(),
            "{action}"
        );
    }
}

#[tokio::test]
async fn extended_window_reconciles_after_reopen() {
    let class = TriggerMessageClass::SignedFirmwareStatusNotification;
    let database = Database::new();
    let store = Store::open(&database.0, 16).unwrap();
    admit_extended(&store, "ExtendedTriggerMessage", class).await;
    put_result(
        &store,
        station_result(class, Some(TriggerNativeResponse::Accepted)),
    )
    .await;
    put_event(&store, station_marker("across-restart", 1, class, 4)).await;
    drop(store);

    let store = Store::open(&database.0, 16).unwrap();
    assert_eq!(
        store
            .trigger_reconciliation_candidates(None, PageLimit::new(10).unwrap())
            .await
            .unwrap(),
        vec![command().request_id]
    );
    let observed = reconcile(&store, 5).await;
    assert_eq!(state(&observed), TriggerObservationStatus::Observed);
    assert_eq!(observed.schema_version.revision, EXTENDED_REVISION);
}
