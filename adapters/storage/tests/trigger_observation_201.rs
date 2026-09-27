use std::path::PathBuf;

use time::{Duration, OffsetDateTime};
use uob_application::{AtomicStoreWrite, OperationalStore, PageLimit};
use uob_contracts::{
    ArtifactDigest, AuthenticatedCommandOrigin, BridgeId, Command, CommandLifecycle,
    CommandOperation, CommandResult, ContractVersion, Environment, EventEnvelope, EventId,
    EventOrigin, EventType, PayloadSchemaId, PrincipalId, PrivilegedOcppOperation,
    ProcessInstanceId, ProtocolActionName, ProtocolEdition, ReleaseId, RequestId, ResourceRef,
    RuntimeIdentity, StationEvent, StationId, TriggerEvse201, TriggerMessageClass,
    TriggerMessageClass201, TriggerNativeResponse201, TriggerNativeStatus201,
    TriggerObservation201, TriggerObservationStatus201, TriggerTarget201, UtcTimestamp,
};
use uob_storage_adapter::SqliteOperationalStore;
use uuid::Uuid;

#[path = "trigger_observation_201/certificate_and_schema.rs"]
mod certificate_and_schema;
#[path = "trigger_observation_201/fixtures.rs"]
mod fixtures;

use fixtures::*;

#[tokio::test]
async fn exact_status_targets_ignore_wrong_station_edition_evse_and_pre_dispatch() {
    let database = Database::new();
    let store = Store::open(&database.0, 16).unwrap();
    let class = TriggerMessageClass201::StatusNotification;
    let command = command("status-201", class, ProtocolEdition::Ocpp201);
    admit(
        &store,
        command.clone(),
        result(
            &command,
            class,
            vec![TriggerTarget201::Connector {
                id: 4,
                connector_id: 2,
            }],
            Some(TriggerEvse201 {
                id: 4,
                connector_id: Some(2),
            }),
            Some(TriggerNativeStatus201::Accepted),
        ),
    )
    .await;

    assert_status_pending_scope(&store, class).await;
    put_nonmatching_status_markers(&store, class).await;
    assert_eq!(
        observation(&reconcile(&store, "status-201", 6).await).status,
        TriggerObservationStatus201::Pending
    );
    put_marker(
        &store,
        marker(
            "right",
            5,
            "charger",
            class,
            TriggerTarget201::Connector {
                id: 4,
                connector_id: 2,
            },
            at(7),
            None,
        ),
    )
    .await;
    let result = reconcile(&store, "status-201", 8).await;
    assert_eq!(
        observation(&result).status,
        TriggerObservationStatus201::Observed
    );
    assert_eq!(observation(&result).observed[0].event_id.as_str(), "right");
    assert_eq!(
        observation(&reconcile(&store, "status-201", 9).await)
            .observed
            .len(),
        1
    );
    assert!(
        !store
            .trigger_pending_for_station_201(station("charger"), class, at(63))
            .await
            .unwrap()
    );
}

async fn assert_status_pending_scope(store: &Store, class: TriggerMessageClass201) {
    assert!(
        store
            .trigger_pending_for_station_201(station("charger"), class, at(2))
            .await
            .unwrap()
    );
    assert!(
        !store
            .trigger_pending_for_station_201(station("other"), class, at(3))
            .await
            .unwrap()
    );
    assert!(
        !store
            .trigger_pending_for_station_201(
                station("charger"),
                TriggerMessageClass201::MeterValues,
                at(3)
            )
            .await
            .unwrap()
    );
    assert!(
        !store
            .trigger_pending_for_station(
                station("charger"),
                TriggerMessageClass::StatusNotification,
                at(3)
            )
            .await
            .unwrap()
    );
    assert!(
        !store
            .trigger_pending_for_station_201(station("charger"), class, at(1))
            .await
            .unwrap()
    );
}

async fn put_nonmatching_status_markers(store: &Store, class: TriggerMessageClass201) {
    put_marker(
        store,
        marker(
            "wrong-station",
            1,
            "other",
            class,
            TriggerTarget201::Connector {
                id: 4,
                connector_id: 2,
            },
            at(3),
            None,
        ),
    )
    .await;
    put_marker(
        store,
        marker(
            "wrong-evse",
            2,
            "charger",
            class,
            TriggerTarget201::Connector {
                id: 5,
                connector_id: 2,
            },
            at(4),
            None,
        ),
    )
    .await;
    put_marker(
        store,
        marker(
            "wrong-connector",
            3,
            "charger",
            class,
            TriggerTarget201::Connector {
                id: 4,
                connector_id: 1,
            },
            at(5),
            None,
        ),
    )
    .await;
    let early = UtcTimestamp::new(at(2).into_inner() - Duration::nanoseconds(1));
    put_marker(
        store,
        marker(
            "early",
            4,
            "charger",
            class,
            TriggerTarget201::Connector {
                id: 4,
                connector_id: 2,
            },
            early,
            None,
        ),
    )
    .await;
}

#[tokio::test]
async fn recovery_merges_external_result_and_preserves_late_reply() {
    let database = Database::new();
    let class = TriggerMessageClass201::MeterValues;
    let command = command("meter-201", class, ProtocolEdition::Ocpp201);
    let scope = Some(TriggerEvse201 {
        id: 4,
        connector_id: Some(2),
    });
    let target = TriggerTarget201::Evse { id: 4 };
    let store = Store::open(&database.0, 16).unwrap();
    admit(
        &store,
        command.clone(),
        result(&command, class, vec![target], scope, None),
    )
    .await;
    put_marker(
        &store,
        marker("meter-effect", 1, "charger", class, target, at(3), None),
    )
    .await;
    let before = reconcile(&store, "meter-201", 4).await;
    assert_eq!(
        observation(&before).status,
        TriggerObservationStatus201::Pending
    );
    drop(store);

    let reopened = Store::open(&database.0, 16).unwrap();
    assert_eq!(
        reopened
            .trigger_reconciliation_candidates(None, PageLimit::new(10).unwrap())
            .await
            .unwrap(),
        vec![command.request_id.clone()]
    );
    let mut uncertain = result(&command, class, vec![target], scope, None);
    uncertain.lifecycle = CommandLifecycle::TransmissionUncertain {
        detail: "restarted".to_owned(),
    };
    put_result(&reopened, uncertain.clone()).await;
    assert_eq!(
        observation(&reconcile(&reopened, "meter-201", 5).await)
            .observed
            .len(),
        1
    );
    let reply = result(
        &command,
        class,
        vec![target],
        scope,
        Some(TriggerNativeStatus201::Accepted),
    );
    put_result(&reopened, reply).await;
    let complete = reconcile(&reopened, "meter-201", 6).await;
    assert_eq!(
        observation(&complete).status,
        TriggerObservationStatus201::Observed
    );
    assert!(matches!(
        complete.lifecycle,
        CommandLifecycle::ProtocolResponse { accepted: true, .. }
    ));
    assert_eq!(
        observation(&complete).observed[0].event_id.as_str(),
        "meter-effect"
    );
    put_result(&reopened, uncertain).await;
    let persisted = reopened
        .command_result_by_request_id(command.request_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        observation(&persisted).status,
        TriggerObservationStatus201::Observed
    );
    assert_eq!(observation(&persisted).observed.len(), 1);
    assert!(matches!(
        persisted.lifecycle,
        CommandLifecycle::ProtocolResponse { accepted: true, .. }
    ));
}

#[tokio::test]
async fn rejected_unsent_trigger_retires_expectation_without_affecting_other_dispatches() {
    let database = Database::new();
    let class = TriggerMessageClass201::MeterValues;
    let target = TriggerTarget201::Evse { id: 4 };
    let scope = Some(TriggerEvse201 {
        id: 4,
        connector_id: None,
    });
    let unsent = command("never-sent-201", class, ProtocolEdition::Ocpp201);
    let store = Store::open(&database.0, 16).unwrap();
    let rejected =
        assert_rejected_unsent_expectation_retired(&store, &unsent, class, target, scope).await;
    drop(store);

    let reopened = Store::open(&database.0, 16).unwrap();
    let dispatched = command("actually-sent-201", class, ProtocolEdition::Ocpp201);
    admit(
        &reopened,
        dispatched.clone(),
        result(&dispatched, class, vec![target], scope, None),
    )
    .await;
    put_marker(
        &reopened,
        marker(
            "shared-meter-effect",
            1,
            "charger",
            class,
            target,
            at(6),
            None,
        ),
    )
    .await;
    assert_eq!(
        reopened
            .trigger_reconciliation_candidates(None, PageLimit::new(10).unwrap())
            .await
            .unwrap(),
        vec![dispatched.request_id.clone()]
    );
    let observed = reconcile(&reopened, "actually-sent-201", 7).await;
    assert_eq!(observation(&observed).observed.len(), 1);
    assert_eq!(
        observation(&observed).observed[0].event_id.as_str(),
        "shared-meter-effect"
    );
    assert_eq!(reconcile(&reopened, "never-sent-201", 7).await, rejected);
    drop(reopened);

    let recovered = Store::open(&database.0, 16).unwrap();
    assert_eq!(
        recovered
            .command_result_by_request_id(unsent.request_id.clone())
            .await
            .unwrap(),
        Some(rejected.clone())
    );
    assert_eq!(reconcile(&recovered, "never-sent-201", 8).await, rejected);
    assert_eq!(
        recovered
            .trigger_reconciliation_candidates(None, PageLimit::new(10).unwrap())
            .await
            .unwrap(),
        vec![dispatched.request_id]
    );
}

async fn assert_rejected_unsent_expectation_retired(
    store: &Store,
    unsent: &Command<String>,
    class: TriggerMessageClass201,
    target: TriggerTarget201,
    scope: Option<TriggerEvse201>,
) -> CommandResult {
    admit(
        store,
        unsent.clone(),
        result(unsent, class, vec![target], scope, None),
    )
    .await;
    assert!(
        store
            .trigger_pending_for_station_201(station("charger"), class, at(3))
            .await
            .unwrap()
    );

    let mut rejected = result(unsent, class, vec![target], scope, None);
    rejected.lifecycle = CommandLifecycle::Rejected {
        error: uob_contracts::CommandError {
            code: uob_contracts::CommandErrorCode::Expired,
            detail: None,
        },
    };
    rejected.recorded_at = at(4);
    rejected.trigger_observation_201 = None;
    put_result(store, rejected.clone()).await;
    assert_eq!(
        store
            .command_result_by_request_id(unsent.request_id.clone())
            .await
            .unwrap(),
        Some(rejected.clone())
    );
    put_result(store, result(unsent, class, vec![target], scope, None)).await;
    assert_eq!(
        store
            .command_result_by_request_id(unsent.request_id.clone())
            .await
            .unwrap(),
        Some(rejected.clone())
    );
    assert!(
        !store
            .trigger_pending_for_station_201(station("charger"), class, at(5))
            .await
            .unwrap()
    );
    assert!(
        store
            .trigger_reconciliation_candidates(None, PageLimit::new(10).unwrap())
            .await
            .unwrap()
            .is_empty()
    );

    rejected
}

#[tokio::test]
async fn deadline_and_original_command_edition_are_enforced() {
    let database = Database::new();
    let store = Store::open(&database.0, 16).unwrap();
    let class = TriggerMessageClass201::Heartbeat;
    let wrong_edition = command("wrong-edition", class, ProtocolEdition::Ocpp16j);
    admit(
        &store,
        wrong_edition.clone(),
        result(
            &wrong_edition,
            class,
            vec![TriggerTarget201::Station],
            None,
            None,
        ),
    )
    .await;
    assert!(
        !store
            .trigger_pending_for_station_201(station("charger"), class, at(3))
            .await
            .unwrap()
    );
    assert!(
        store
            .reconcile_trigger_observation(wrong_edition.request_id.clone(), at(3))
            .await
            .is_err()
    );

    let right = command("deadline-201", class, ProtocolEdition::Ocpp201);
    admit(
        &store,
        right.clone(),
        result(
            &right,
            class,
            vec![TriggerTarget201::Station],
            None,
            Some(TriggerNativeStatus201::Accepted),
        ),
    )
    .await;
    put_marker(
        &store,
        marker(
            "too-late",
            1,
            "charger",
            class,
            TriggerTarget201::Station,
            at(63),
            None,
        ),
    )
    .await;
    assert_eq!(
        observation(&reconcile(&store, "deadline-201", 63).await).status,
        TriggerObservationStatus201::Absent
    );
    assert_eq!(
        store
            .trigger_reconciliation_candidates(None, PageLimit::new(10).unwrap())
            .await
            .unwrap(),
        vec![wrong_edition.request_id]
    );
}
