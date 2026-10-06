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
        let query = "SELECT (SELECT group_concat(payload) FROM reservations16)
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
        "UPDATE reservations16 SET payload=json_replace(payload,'$.revision',9) WHERE request_id='owned'",
        "UPDATE reservations16 SET payload=json_replace(payload,'$.state','cancelled') WHERE request_id='owned'",
        "UPDATE reservations16 SET payload=json_replace(payload,'$.changed_at','1970-01-01T00:00:08Z') WHERE request_id='owned'",
        "UPDATE commands SET payload=json_replace(payload,'$.operation.parameters.payload.reservationId',7) WHERE request_id='owned'",
        "UPDATE commands SET payload=json_replace(payload,'$.operation.parameters.payload.connectorId',2) WHERE request_id='owned'",
        "DELETE FROM reservations16 WHERE request_id='owned'",
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
async fn mixed_or_lifecycle_inconsistent_evidence_never_changes_the_reservation() {
    let database = Database::new();
    let store = database.open();
    let command = reserve(&store, "owned", -5, Some(candidate(1, 1)), 1)
        .await
        .unwrap();
    dispatched(&store, &command, 2).await;
    let evidence = ReservationResult16::ReserveNow {
        reservation_id: -5,
        connector_id: 1,
        status: Some(ReserveNowStatus16::Accepted),
        reconciliation: ReservationReconciliation16 {
            revision: 0,
            state: ReservationState16::Pending,
            observed_at: at(3),
            source_time: None,
        },
    };
    let native = CommandLifecycle::ProtocolResponse {
        accepted: true,
        error: None,
    };
    let mut early = result(&command, CommandLifecycle::Dispatched, 3);
    early.reservation_16 = Some(evidence.clone());
    early.schema_version = ContractVersion::V1_RESERVATION_16;
    let mut mixed = result(&command, native.clone(), 3);
    mixed.reservation_16 = Some(evidence.clone());
    mixed.schema_version = ContractVersion::V1_RESERVATION_16;
    mixed.local_authorization_16 = Some(LocalAuthorizationResult16::ClearCache {
        status: ClearCacheStatus16::Accepted,
    });
    let mut stale = result(&command, native.clone(), 3);
    stale.reservation_16 = Some(evidence.clone());
    let mut disagreeing = result(&command, native, 3);
    disagreeing.schema_version = ContractVersion::V1_RESERVATION_16;
    let mut occupied = evidence;
    if let ReservationResult16::ReserveNow { status, .. } = &mut occupied {
        *status = Some(ReserveNowStatus16::Occupied);
    }
    disagreeing.reservation_16 = Some(occupied);
    for invalid in [early, mixed, stale, disagreeing] {
        let mut write = AtomicStoreWrite::empty();
        write.command_result = Some(invalid);
        assert!(store.write_atomic(write).await.is_err());
    }
    let record = records(&store).await.remove(0);
    assert_eq!(record.state, ReservationState16::Pending);
    assert!(record.unresolved);
    let stored = store
        .command_result_by_request_id(command.request_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.lifecycle, CommandLifecycle::Dispatched);
    assert!(matches!(
        stored.reservation_16,
        Some(ReservationResult16::ReserveNow { status: None, .. })
    ));
    response(&store, &command, "Accepted", 4).await;
    assert_eq!(records(&store).await[0].state, ReservationState16::Active);
}
