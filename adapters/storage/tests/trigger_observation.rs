use std::path::PathBuf;

use time::{Duration, OffsetDateTime};
use uob_application::{AtomicStoreWrite, OperationalStore, PageLimit};
use uob_contracts::{
    ArtifactDigest, AuthenticatedCommandOrigin, BridgeId, Command, CommandLifecycle,
    CommandOperation, CommandResult, ContractVersion, Environment, EventEnvelope, EventId,
    EventOrigin, EventType, PayloadSchemaId, PrincipalId, PrivilegedOcppOperation,
    ProcessInstanceId, ProtocolActionName, ProtocolEdition, ReleaseId, RequestId, ResourceRef,
    RuntimeIdentity, StationEvent, StationId, TriggerMessageClass, TriggerNativeResponse,
    TriggerObservation, TriggerObservationStatus, UtcTimestamp,
};
use uob_storage_adapter::SqliteOperationalStore;
use uuid::Uuid;

#[path = "trigger_observation/fixtures.rs"]
mod fixtures;

use fixtures::*;

#[tokio::test]
async fn journal_first_response_later_and_restart_keep_distinct_evidence() {
    let database = Database::new();
    let store = Store::open(&database.0, 16).unwrap();
    admitted(&store).await;
    assert!(
        store
            .trigger_pending_for_station(station(), TriggerMessageClass::StatusNotification, at(3))
            .await
            .unwrap()
    );
    assert!(
        !store
            .trigger_pending_for_station(station(), TriggerMessageClass::MeterValues, at(3))
            .await
            .unwrap()
    );
    put_event(
        &store,
        event("first", 1, 1, TriggerMessageClass::StatusNotification, 3),
    )
    .await;
    let pending = store
        .reconcile_trigger_observation(command().request_id, at(4))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state(&pending), TriggerObservationStatus::Pending);
    assert_eq!(pending.trigger_observation.unwrap().observed.len(), 1);
    drop(store);

    let store = Store::open(&database.0, 16).unwrap();
    let candidates = store
        .trigger_reconciliation_candidates(None, PageLimit::new(10).unwrap())
        .await
        .unwrap();
    assert_eq!(candidates, vec![command().request_id]);
    put_result(&store, result(Some(TriggerNativeResponse::Accepted))).await;
    let partial = store
        .reconcile_trigger_observation(command().request_id, at(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state(&partial), TriggerObservationStatus::Partial);
    put_event(
        &store,
        event("wrong-class", 2, 2, TriggerMessageClass::MeterValues, 6),
    )
    .await;
    let still_partial = store
        .reconcile_trigger_observation(command().request_id, at(6))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        still_partial
            .trigger_observation
            .as_ref()
            .unwrap()
            .observed
            .len(),
        1
    );
    put_event(
        &store,
        event("second", 3, 2, TriggerMessageClass::StatusNotification, 7),
    )
    .await;
    let observed = store
        .reconcile_trigger_observation(command().request_id, at(8))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state(&observed), TriggerObservationStatus::Observed);
    assert_eq!(
        observed
            .trigger_observation
            .as_ref()
            .unwrap()
            .observed
            .len(),
        2
    );
    let duplicate = store
        .reconcile_trigger_observation(command().request_id, at(9))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(duplicate, observed);
    assert!(matches!(
        duplicate.lifecycle,
        CommandLifecycle::ProtocolResponse { accepted: true, .. }
    ));
}

#[tokio::test]
async fn accepted_reply_stays_pending_until_compatible_post_dispatch_station_event() {
    let database = Database::new();
    let store = Store::open(&database.0, 16).unwrap();
    admitted(&store).await;
    put_event(
        &store,
        event(
            "before-dispatch",
            1,
            1,
            TriggerMessageClass::StatusNotification,
            1,
        ),
    )
    .await;
    put_result(&store, result(Some(TriggerNativeResponse::Accepted))).await;
    let pending = store
        .reconcile_trigger_observation(command().request_id, at(3))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state(&pending), TriggerObservationStatus::Pending);
    assert!(pending.trigger_observation.unwrap().observed.is_empty());
    put_event(
        &store,
        event(
            "after-dispatch",
            2,
            1,
            TriggerMessageClass::StatusNotification,
            4,
        ),
    )
    .await;
    let partial = store
        .reconcile_trigger_observation(command().request_id, at(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state(&partial), TriggerObservationStatus::Partial);
    assert_eq!(partial.trigger_observation.unwrap().observed.len(), 1);
}

#[tokio::test]
async fn expired_partial_is_reconciled_once_and_excluded_from_future_sweeps() {
    let database = Database::new();
    let store = Store::open(&database.0, 16).unwrap();
    admitted(&store).await;
    put_result(&store, result(Some(TriggerNativeResponse::Accepted))).await;
    put_event(
        &store,
        event(
            "one-target",
            1,
            1,
            TriggerMessageClass::StatusNotification,
            4,
        ),
    )
    .await;
    let partial = store
        .reconcile_trigger_observation(command().request_id.clone(), at(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state(&partial), TriggerObservationStatus::Partial);
    assert_eq!(
        store
            .trigger_reconciliation_candidates(None, PageLimit::new(10).unwrap())
            .await
            .unwrap(),
        vec![command().request_id]
    );
    let finalized = store
        .reconcile_trigger_observation(command().request_id, at(63))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state(&finalized), TriggerObservationStatus::Partial);
    assert_eq!(
        finalized
            .trigger_observation
            .as_ref()
            .unwrap()
            .observed
            .len(),
        1
    );
    assert!(
        store
            .trigger_reconciliation_candidates(None, PageLimit::new(10).unwrap())
            .await
            .unwrap()
            .is_empty()
    );
    drop(store);
    let reopened = Store::open(&database.0, 16).unwrap();
    assert!(
        reopened
            .trigger_reconciliation_candidates(None, PageLimit::new(10).unwrap())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !reopened
            .trigger_pending_for_station(station(), TriggerMessageClass::StatusNotification, at(5))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn submillisecond_pre_dispatch_marker_is_not_success_evidence() {
    let database = Database::new();
    let store = Store::open(&database.0, 16).unwrap();
    admitted(&store).await;
    put_result(&store, result(Some(TriggerNativeResponse::Accepted))).await;
    let mut early = event("early", 1, 1, TriggerMessageClass::StatusNotification, 2);
    early.observed_at = UtcTimestamp::new(at(2).into_inner() - Duration::nanoseconds(1));
    put_event(&store, early).await;
    let pending = store
        .reconcile_trigger_observation(command().request_id, at(3))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state(&pending), TriggerObservationStatus::Pending);
    let mut later = event("later", 2, 1, TriggerMessageClass::StatusNotification, 2);
    later.observed_at = UtcTimestamp::new(at(2).into_inner() + Duration::nanoseconds(1));
    put_event(&store, later).await;
    let partial = store
        .reconcile_trigger_observation(command().request_id, at(3))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state(&partial), TriggerObservationStatus::Partial);
    assert_eq!(
        partial.trigger_observation.unwrap().observed[0]
            .event_id
            .as_str(),
        "later"
    );
}

#[tokio::test]
async fn deadline_without_connection_does_not_turn_ack_or_missing_reply_into_success() {
    let database = Database::new();
    let store = Store::open(&database.0, 16).unwrap();
    admitted(&store).await;
    let absent = store
        .reconcile_trigger_observation(command().request_id, at(63))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state(&absent), TriggerObservationStatus::Absent);
    assert_eq!(
        absent.trigger_observation.as_ref().unwrap().native_response,
        None
    );
    put_result(&store, result(Some(TriggerNativeResponse::Accepted))).await;
    let accepted_but_absent = store
        .reconcile_trigger_observation(command().request_id, at(64))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        state(&accepted_but_absent),
        TriggerObservationStatus::Absent
    );
    assert_eq!(
        accepted_but_absent
            .trigger_observation
            .unwrap()
            .native_response,
        Some(TriggerNativeResponse::Accepted)
    );
    assert!(
        !store
            .trigger_pending_for_station(station(), TriggerMessageClass::StatusNotification, at(64))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn uncertain_recovery_and_late_correlated_response_preserve_exact_lifecycle() {
    let database = Database::new();
    let store = Store::open(&database.0, 16).unwrap();
    admitted(&store).await;
    let mut recovered = result(None);
    recovered.lifecycle = CommandLifecycle::TransmissionUncertain {
        detail: "restarted without correlated reply".to_owned(),
    };
    put_result(&store, recovered.clone()).await;
    let uncertain = store
        .reconcile_trigger_observation(command().request_id, at(3))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        uncertain.lifecycle,
        CommandLifecycle::TransmissionUncertain { .. }
    ));
    assert_eq!(state(&uncertain), TriggerObservationStatus::Pending);
    put_result(&store, result(Some(TriggerNativeResponse::Accepted))).await;
    let accepted = store
        .reconcile_trigger_observation(command().request_id, at(4))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        accepted.lifecycle,
        CommandLifecycle::ProtocolResponse { accepted: true, .. }
    ));
    assert_eq!(state(&accepted), TriggerObservationStatus::Pending);
    put_result(&store, recovered).await;
    let unchanged = store
        .command_result_by_request_id(command().request_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        unchanged.lifecycle,
        CommandLifecycle::ProtocolResponse { accepted: true, .. }
    ));
    assert_eq!(
        unchanged.trigger_observation.unwrap().native_response,
        Some(TriggerNativeResponse::Accepted)
    );
}

#[tokio::test]
async fn native_not_implemented_cannot_become_observed_from_a_later_message() {
    let database = Database::new();
    let store = Store::open(&database.0, 16).unwrap();
    admitted(&store).await;
    put_result(&store, result(Some(TriggerNativeResponse::NotImplemented))).await;
    put_event(
        &store,
        event(
            "unrelated-later",
            1,
            1,
            TriggerMessageClass::StatusNotification,
            3,
        ),
    )
    .await;
    let result = store
        .reconcile_trigger_observation(command().request_id, at(4))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state(&result), TriggerObservationStatus::Unsupported);
    assert_eq!(result.trigger_observation.unwrap().observed.len(), 1);
}
