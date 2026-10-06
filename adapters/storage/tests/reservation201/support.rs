use serde_json::{Value, json};
use std::path::PathBuf;
pub use uob_application::*;
pub use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;
pub type Store = SqliteOperationalStore<Value, String, String, String>;
pub struct Database(pub PathBuf);
impl Database {
    pub fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-reserve201-{}.db", uuid::Uuid::new_v4())))
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
pub fn evse(id: u32) -> ResourceRef {
    let mut resource = station();
    resource.resource = Some(CanonicalResource::Evse {
        evse_id: CanonicalEvseId::new(format!("evse-{id}")).unwrap(),
        connector_id: None,
    });
    resource.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
        evse_id: id,
        connector_id: None,
    });
    resource
}
/// `evse_id` 0 selects an unspecified-EVSE reservation.
pub fn candidate(evse_id: u32, key: u8) -> ReservationCandidate201 {
    ReservationCandidate201 {
        evse_id: (evse_id > 0).then_some(evse_id),
        connector_type: None,
        expiry_date_time: at(1000),
        token_key: ReservationKey201([key; 32]),
        group_key: Some(ReservationKey201([9; 32])),
    }
}
pub fn command(
    id: &str,
    reservation_id: i32,
    candidate: Option<&ReservationCandidate201>,
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
        if let Some(id) = candidate.evse_id {
            value.resource = evse(id);
        }
        let mut payload = json!({"id":reservation_id,"expiryDateTime":candidate.expiry_date_time,"reservationReference":format!("reserve201:{}", "a".repeat(64))});
        if let Some(id) = candidate.evse_id {
            payload["evseId"] = json!(id);
        }
        if let Some(kind) = candidate.connector_type {
            payload["connectorType"] = json!(kind);
        }
        (
            "ReserveNow",
            RESERVE_NOW_REFERENCE_SCHEMA_201.to_owned(),
            payload,
        )
    } else {
        (
            "CancelReservation",
            CANCEL_RESERVATION_SCHEMA_201.to_owned(),
            json!({"reservationId":reservation_id}),
        )
    };
    value.operation = CommandOperation::Ocpp(PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp201,
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
    candidate: Option<ReservationCandidate201>,
    now: i64,
) -> Result<Command<Value>, StorageError> {
    let command = command(id, reservation_id, candidate.as_ref(), now);
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command.clone());
    write.command_result = Some(result(&command, CommandLifecycle::Admitted, now));
    write.reservation_201 = Some(Box::new(ReservationMutation201 {
        station: station(),
        request_id: command.request_id.clone(),
        reservation_id,
        admitted_at: at(now),
        generation: 1,
        mutation: candidate.map_or(
            ReservationMutationKind201::Cancel,
            ReservationMutationKind201::Reserve,
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
pub fn native(command: &Command<Value>, status: &str, now: i64) -> CommandResult {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        panic!("native reservation")
    };
    let reconciliation = ReservationReconciliation201 {
        revision: 0,
        state: ReservationState201::Pending,
        observed_at: at(now),
        source_time: None,
    };
    let evidence = if operation.action.as_str() == "ReserveNow" {
        ReservationResult201::ReserveNow {
            reservation_id: i32::try_from(operation.payload["id"].as_i64().unwrap()).unwrap(),
            evse_id: operation.payload["evseId"]
                .as_u64()
                .map(|id| u32::try_from(id).unwrap()),
            status: Some(serde_json::from_value(json!(status)).unwrap()),
            reconciliation,
        }
    } else {
        ReservationResult201::CancelReservation {
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
            error: (!evidence.accepted()).then_some(CommandError {
                code: CommandErrorCode::ProtocolRejected,
                detail: None,
            }),
        },
        now,
    );
    value.reservation_201 = Some(evidence);
    value.schema_version = ContractVersion::V1_RESERVATION_201;
    value
}
pub async fn response(store: &Store, command: &Command<Value>, status: &str, now: i64) {
    persist(store, native(command, status, now)).await;
}
pub async fn observe(store: &Store, kind: ReservationObservationKind201, now: i64) {
    let mut write = AtomicStoreWrite::empty();
    write
        .reservation_observations_201
        .push(ReservationObservation201 {
            station: station(),
            observed_at: at(now),
            kind,
        });
    store.write_atomic(write).await.unwrap();
}
pub fn transaction(
    id: i32,
    evse: u32,
    token: Option<u8>,
    group: Option<u8>,
    source: i64,
) -> ReservationObservationKind201 {
    ReservationObservationKind201::Transaction {
        reservation_id: id,
        evse_id: evse,
        token_key: token.map(|value| ReservationKey201([value; 32])),
        group_key: group.map(|value| ReservationKey201([value; 32])),
        source_time: at(source),
    }
}
pub fn update(id: i32, status: ReservationUpdateStatus201) -> ReservationObservationKind201 {
    ReservationObservationKind201::StatusUpdate {
        reservation_id: id,
        status,
    }
}
pub async fn records(store: &Store) -> Vec<ReservationRecord201> {
    store.reservations_201(station()).await.unwrap()
}
pub async fn evidence(store: &Store, command: &Command<Value>) -> ReservationResult201 {
    store
        .command_result_by_request_id(command.request_id.clone())
        .await
        .unwrap()
        .unwrap()
        .reservation_201
        .unwrap()
}
pub fn state(evidence: &ReservationResult201) -> ReservationState201 {
    match evidence {
        ReservationResult201::ReserveNow { reconciliation, .. }
        | ReservationResult201::CancelReservation { reconciliation, .. } => reconciliation.state,
    }
}
