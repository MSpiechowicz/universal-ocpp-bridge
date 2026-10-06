use super::support::*;
use serde_json::Value;

async fn accepted(database: &Database) -> Command<Value> {
    let store = database.open();
    let command = reserve(&store, "owned", i32::MAX, Some(candidate(1, 1)), 1)
        .await
        .unwrap();
    dispatched(&store, &command, 2).await;
    response(&store, &command, "Accepted", 3).await;
    command
}

fn tamper(database: &Database, statement: &str) {
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    let rows = |connection: &rusqlite::Connection| -> String {
        let query = "SELECT (SELECT group_concat(payload) FROM reservations201)
            || (SELECT group_concat(payload) FROM commands)";
        connection
            .query_row(query, [], |row| row.get::<_, Option<String>>(0))
            .unwrap()
            .unwrap_or_default()
    };
    let before = rows(&connection);
    assert_eq!(connection.execute(statement, []).unwrap(), 1, "{statement}");
    // A JSON path that does not exist would add an unrelated key instead of tampering.
    assert_ne!(rows(&connection), before, "{statement}");
}

#[tokio::test]
async fn stored_evidence_must_still_agree_with_its_durable_owner_and_immutable_action() {
    for statement in [
        "UPDATE reservations201 SET payload=json_replace(payload,'$.revision',9) WHERE request_id='owned'",
        "UPDATE reservations201 SET payload=json_replace(payload,'$.state','removed') WHERE request_id='owned'",
        "UPDATE reservations201 SET payload=json_replace(payload,'$.changed_at','1970-01-01T00:00:08Z') WHERE request_id='owned'",
        "UPDATE commands SET payload=json_replace(payload,'$.operation.parameters.payload.id',7) WHERE request_id='owned'",
        "UPDATE commands SET payload=json_replace(payload,'$.operation.parameters.payload.evseId',2) WHERE request_id='owned'",
        "UPDATE commands SET payload=json_replace(payload,'$.operation.parameters.protocol','ocpp16j') WHERE request_id='owned'",
        "DELETE FROM reservations201 WHERE request_id='owned'",
    ] {
        let database = Database::new();
        let command = accepted(&database).await;
        tamper(&database, statement);
        let store = database.open();
        let error = store
            .command_result_by_request_id(command.request_id.clone())
            .await
            .expect_err(statement);
        assert_eq!(
            error.code(),
            StorageErrorCode::IntegrityFailure,
            "{statement}"
        );
    }
}

#[tokio::test]
async fn mixed_cross_edition_or_lifecycle_inconsistent_evidence_never_changes_the_reservation() {
    let database = Database::new();
    let store = database.open();
    let command = reserve(&store, "owned", -5, Some(candidate(1, 1)), 1)
        .await
        .unwrap();
    dispatched(&store, &command, 2).await;
    let native = native(&command, "Accepted", 3);
    let mut early = native.clone();
    early.lifecycle = CommandLifecycle::Dispatched;
    let mut mixed = native.clone();
    mixed.local_authorization_201 = Some(LocalAuthorizationResult201::ClearCache {
        status: ClearCacheStatus201::Accepted,
    });
    let mut cross = native.clone();
    cross.reservation_16 = Some(ReservationResult16::CancelReservation {
        reservation_id: -5,
        status: Some(CancelReservationStatus16::Accepted),
        reconciliation: ReservationReconciliation16 {
            revision: 0,
            state: ReservationState16::Pending,
            observed_at: at(3),
            source_time: None,
        },
    });
    let mut stale = native.clone();
    stale.schema_version = ContractVersion::V1_RESERVATION_16;
    let mut moved = native.clone();
    if let Some(ReservationResult201::ReserveNow { evse_id, .. }) = &mut moved.reservation_201 {
        *evse_id = Some(2);
    }
    let mut disagreeing = native;
    if let CommandLifecycle::ProtocolResponse { accepted, .. } = &mut disagreeing.lifecycle {
        *accepted = false;
    }
    for invalid in [early, mixed, cross, stale, moved, disagreeing] {
        let mut write = AtomicStoreWrite::empty();
        write.command_result = Some(invalid);
        assert!(store.write_atomic(write).await.is_err());
    }
    let record = records(&store).await.remove(0);
    assert_eq!(record.state, ReservationState201::Pending);
    assert!(record.unresolved);
    let stored = store
        .command_result_by_request_id(command.request_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.lifecycle, CommandLifecycle::Dispatched);
    assert!(matches!(
        stored.reservation_201,
        Some(ReservationResult201::ReserveNow { status: None, .. })
    ));
    assert!(stored.reservation_16.is_none());
    response(&store, &command, "Accepted", 4).await;
    assert_eq!(records(&store).await[0].state, ReservationState201::Active);
}

#[tokio::test]
async fn edition_validators_reject_foreign_scopes_before_admission() {
    let database = Database::new();
    let store = database.open();
    let mut wrong_scope = command("wrong-scope", 1, Some(&candidate(1, 1)), 1);
    wrong_scope.resource = station();
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(wrong_scope.clone());
    write.command_result = Some(result(&wrong_scope, CommandLifecycle::Admitted, 1));
    assert_eq!(
        store.write_atomic(write).await.unwrap_err().code(),
        StorageErrorCode::Conflict,
        "an EVSE reservation cannot be admitted at station scope"
    );
    let mut cancel_on_evse = command("cancel-evse", 1, None, 1);
    cancel_on_evse.resource = evse(1);
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(cancel_on_evse.clone());
    write.command_result = Some(result(&cancel_on_evse, CommandLifecycle::Admitted, 1));
    assert!(store.write_atomic(write).await.is_err());
    assert!(
        reserve(&store, "valid", 1, Some(candidate(1, 1)), 2)
            .await
            .is_ok()
    );
}
