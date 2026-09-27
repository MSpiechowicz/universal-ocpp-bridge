use super::*;

#[tokio::test]
async fn station_certificate_ignores_supplied_evse_and_rejects_immutable_expectation_rewrite() {
    let database = Database::new();
    let store = Store::open(&database.0, 16).unwrap();
    let class = TriggerMessageClass201::SignChargingStationCertificate;
    let request = command("station-cert", class, ProtocolEdition::Ocpp201);
    let scope = Some(TriggerEvse201 {
        id: 5,
        connector_id: Some(2),
    });
    let original = result(
        &request,
        class,
        vec![TriggerTarget201::Station],
        scope,
        Some(TriggerNativeStatus201::Accepted),
    );
    admit(&store, request.clone(), original.clone()).await;
    let mut rewritten = original.clone();
    rewritten
        .trigger_observation_201
        .as_mut()
        .unwrap()
        .expected_targets = vec![TriggerTarget201::Evse { id: 5 }];
    assert!(
        store
            .write_atomic(AtomicStoreWrite::<String, StationEvent, (), ()> {
                command_result: Some(rewritten),
                ..AtomicStoreWrite::empty()
            })
            .await
            .is_err()
    );
    put_marker(
        &store,
        marker(
            "station-certificate",
            1,
            "charger",
            class,
            TriggerTarget201::Station,
            at(3),
            None,
        ),
    )
    .await;
    let result = reconcile(&store, "station-cert", 4).await;
    assert_eq!(
        observation(&result).status,
        TriggerObservationStatus201::Observed
    );
    assert_eq!(
        observation(&result).observed[0].target,
        TriggerTarget201::Station
    );
}

#[test]
fn newer_schema_is_not_downgraded_or_rewritten() {
    let database = Database::new();
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    connection.execute_batch("PRAGMA user_version = 12; CREATE TABLE preserved(value TEXT); INSERT INTO preserved VALUES('unchanged');").unwrap();
    drop(connection);

    assert!(Store::open(&database.0, 16).is_err());
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let preserved: String = connection
        .query_row("SELECT value FROM preserved", [], |row| row.get(0))
        .unwrap();
    assert_eq!((version, preserved.as_str()), (12, "unchanged"));
}

#[tokio::test]
async fn scoped_certificate_receipt_is_unattributable_and_transaction_requires_trigger_reason() {
    let database = Database::new();
    let store = Store::open(&database.0, 16).unwrap();

    assert_scoped_certificate_receipt(&store).await;
    assert_triggered_transaction_receipt(&store).await;
}

async fn assert_scoped_certificate_receipt(store: &Store) {
    let certificate = TriggerMessageClass201::SignV2GCertificate;
    let request = command("cert-201", certificate, ProtocolEdition::Ocpp201);
    admit(
        store,
        request.clone(),
        result(
            &request,
            certificate,
            vec![TriggerTarget201::Evse { id: 4 }],
            Some(TriggerEvse201 {
                id: 4,
                connector_id: None,
            }),
            Some(TriggerNativeStatus201::Accepted),
        ),
    )
    .await;
    put_marker(
        store,
        marker(
            "certificate",
            1,
            "charger",
            certificate,
            TriggerTarget201::Station,
            at(3),
            None,
        ),
    )
    .await;
    let certificate_result = reconcile(store, "cert-201", 4).await;
    assert_eq!(
        observation(&certificate_result).status,
        TriggerObservationStatus201::Unattributable
    );
    assert_eq!(
        observation(&certificate_result).observed[0].target,
        TriggerTarget201::Station
    );
}

async fn assert_triggered_transaction_receipt(store: &Store) {
    let transaction = TriggerMessageClass201::TransactionEvent;
    let request = command("transaction-201", transaction, ProtocolEdition::Ocpp201);
    admit(
        store,
        request.clone(),
        result(
            &request,
            transaction,
            vec![TriggerTarget201::Evse { id: 4 }],
            Some(TriggerEvse201 {
                id: 4,
                connector_id: None,
            }),
            Some(TriggerNativeStatus201::Accepted),
        ),
    )
    .await;
    put_marker(
        store,
        marker(
            "ordinary-transaction",
            2,
            "charger",
            transaction,
            TriggerTarget201::Connector {
                id: 4,
                connector_id: 2,
            },
            at(4),
            Some("Authorized"),
        ),
    )
    .await;
    assert_eq!(
        observation(&reconcile(store, "transaction-201", 5).await).status,
        TriggerObservationStatus201::Pending
    );
    put_marker(
        store,
        marker(
            "triggered-transaction",
            3,
            "charger",
            transaction,
            TriggerTarget201::Connector {
                id: 4,
                connector_id: 2,
            },
            at(6),
            Some("Trigger"),
        ),
    )
    .await;
    let result = reconcile(store, "transaction-201", 7).await;
    assert_eq!(
        observation(&result).status,
        TriggerObservationStatus201::Observed
    );
    assert_eq!(
        observation(&result).observed[0].event_id.as_str(),
        "triggered-transaction"
    );
}
