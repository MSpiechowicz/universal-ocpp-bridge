use serde_json::{Value, json};
use std::path::PathBuf;
pub use uob_application::*;
pub use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;
pub type Store = SqliteOperationalStore<Value, String, String, String>;
pub struct Database(pub PathBuf);
impl Database {
    pub fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-reserve16-{}.db", uuid::Uuid::new_v4())))
    }
    pub fn open(&self) -> Store {
        Store::open(&self.0, 32).unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
pub fn at(second: i64) -> UtcTimestamp {
    UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(second))
}
pub fn station() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge").unwrap(),
        station_id: StationId::new("station").unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}
pub fn candidate(connector_id: u32, key: u8) -> ReservationCandidate16 {
    ReservationCandidate16 {
        connector_id,
        expiry_date: at(1000),
        token_key: ReservationKey16([key; 32]),
        group_key: Some(ReservationKey16([9; 32])),
    }
}
pub fn command(
    id: &str,
    reservation_id: i32,
    candidate: Option<&ReservationCandidate16>,
    now: i64,
) -> Command<Value> {
    let mut value: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    value.request_id = RequestId::new(id).unwrap();
    value.correlation_id = Some(CorrelationId::new(id).unwrap());
    value.resource = station();
    value.admitted_at = at(now);
    value.expires_at = at(now + 1000);
    let (action, schema, payload) = if let Some(candidate) = candidate {
        if candidate.connector_id > 0 {
            value.resource.resource = Some(CanonicalResource::Connector {
                connector_id: CanonicalConnectorId::new(format!(
                    "connector-{}",
                    candidate.connector_id
                ))
                .unwrap(),
            });
            value.resource.native_protocol_reference = Some(NativeProtocolReference::Ocpp16 {
                connector_id: candidate.connector_id,
            });
        }
        (
            "ReserveNow",
            RESERVE_NOW_REFERENCE_SCHEMA_16.to_owned(),
            json!({"connectorId":candidate.connector_id,"expiryDate":candidate.expiry_date,"reservationId":reservation_id,"reservationReference":format!("reserve16:{}", "a".repeat(64))}),
        )
    } else {
        (
            "CancelReservation",
            "urn:OCPP:1.6:2019:12:CancelReservationRequest".to_owned(),
            json!({"reservationId":reservation_id}),
        )
    };
    value.operation = CommandOperation::Ocpp(PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp16j,
        action: ProtocolActionName::new(action).unwrap(),
        payload_schema: PayloadSchemaId::new(schema).unwrap(),
        payload,
    });
    value
}
pub fn result(command: &Command<Value>, lifecycle: CommandLifecycle, now: i64) -> CommandResult {
    let old: Value = serde_json::from_str(include_str!(
        "../../../../crates/contracts/tests/fixtures/command-results-v1.json"
    ))
    .unwrap();
    let mut value: CommandResult = serde_json::from_value(old[0].clone()).unwrap();
    value.resource.clone_from(&command.resource);
    value.correlation_id.clone_from(&command.correlation_id);
    value.return_route = command.return_route();
    value.lifecycle = lifecycle;
    value.recorded_at = at(now);
    value
}
pub async fn reserve(
    store: &Store,
    id: &str,
    reservation_id: i32,
    candidate: Option<ReservationCandidate16>,
    now: i64,
) -> Result<Command<Value>, StorageError> {
    let command = command(id, reservation_id, candidate.as_ref(), now);
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command.clone());
    write.command_result = Some(result(&command, CommandLifecycle::Admitted, now));
    write.reservation_16 = Some(Box::new(ReservationMutation16 {
        station: station(),
        request_id: command.request_id.clone(),
        reservation_id,
        admitted_at: at(now),
        generation: 1,
        mutation: candidate.map_or(
            ReservationMutationKind16::Cancel,
            ReservationMutationKind16::Reserve,
        ),
    }));
    store.write_atomic(write).await?;
    Ok(command)
}
pub async fn persist(store: &Store, result: CommandResult) {
    let mut write = AtomicStoreWrite::empty();
    write.command_result = Some(result);
    store.write_atomic(write).await.unwrap();
}
pub async fn dispatched(store: &Store, command: &Command<Value>, now: i64) {
    persist(store, result(command, CommandLifecycle::Dispatched, now)).await;
}
pub async fn response(store: &Store, command: &Command<Value>, status: &str, now: i64) {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        panic!("native reservation")
    };
    let reconciliation = ReservationReconciliation16 {
        revision: 0,
        state: ReservationState16::Pending,
        observed_at: at(now),
        source_time: None,
    };
    let evidence = if operation.action.as_str() == "ReserveNow" {
        ReservationResult16::ReserveNow {
            reservation_id: i32::try_from(operation.payload["reservationId"].as_i64().unwrap())
                .unwrap(),
            connector_id: u32::try_from(operation.payload["connectorId"].as_u64().unwrap())
                .unwrap(),
            status: Some(serde_json::from_value(json!(status)).unwrap()),
            reconciliation,
        }
    } else {
        ReservationResult16::CancelReservation {
            reservation_id: i32::try_from(operation.payload["reservationId"].as_i64().unwrap())
                .unwrap(),
            status: Some(serde_json::from_value(json!(status)).unwrap()),
            reconciliation,
        }
    };
    let mut value = result(
        command,
        CommandLifecycle::ProtocolResponse {
            accepted: evidence.accepted(),
            error: None,
        },
        now,
    );
    value.reservation_16 = Some(evidence);
    value.schema_version = ContractVersion::V1_RESERVATION_16;
    persist(store, value).await;
}
pub async fn observe(store: &Store, kind: ReservationObservationKind16, now: i64) {
    let mut write = AtomicStoreWrite::empty();
    write
        .reservation_observations_16
        .push(ReservationObservation16 {
            station: station(),
            observed_at: at(now),
            kind,
        });
    store.write_atomic(write).await.unwrap();
}
pub fn start(
    id: i32,
    connector: u32,
    token: u8,
    parent: Option<u8>,
    source: i64,
) -> ReservationObservationKind16 {
    ReservationObservationKind16::Start {
        reservation_id: id,
        connector_id: connector,
        token_key: ReservationKey16([token; 32]),
        group_key: parent.map(|value| ReservationKey16([value; 32])),
        source_time: at(source),
    }
}
pub async fn records(store: &Store) -> Vec<ReservationRecord16> {
    store.reservations_16(station()).await.unwrap()
}
pub async fn evidence(store: &Store, command: &Command<Value>) -> ReservationResult16 {
    store
        .command_result_by_request_id(command.request_id.clone())
        .await
        .unwrap()
        .unwrap()
        .reservation_16
        .unwrap()
}
