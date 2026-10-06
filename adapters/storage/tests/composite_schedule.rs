use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use uob_application::{AtomicStoreWrite, OperationalStore};
use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;

type Store = SqliteOperationalStore<Value, String, String, String>;
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-composite-{}.db", uuid::Uuid::new_v4())))
    }
    fn open(&self) -> Store {
        Store::open(&self.0, 16).unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
fn at(value: &str) -> UtcTimestamp {
    serde_json::from_value(json!(value)).unwrap()
}

async fn admit(store: &Store, id: &str) -> Command<Value> {
    let mut command: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    command.request_id = RequestId::new(id).unwrap();
    command.operation = CommandOperation::Ocpp(PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp16j,
        action: ProtocolActionName::new("GetCompositeSchedule").unwrap(),
        payload_schema: PayloadSchemaId::new("urn:OCPP:1.6:2019:12:GetCompositeScheduleRequest")
            .unwrap(),
        payload: json!({"connectorId":0,"duration":60}),
    });
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command.clone());
    store.write_atomic(write).await.unwrap();
    command
}
fn result(command: &Command<Value>, lifecycle: CommandLifecycle) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: command.correlation_id.clone(),
        resource: command.resource.clone(),
        return_route: command.return_route(),
        recorded_at: command.admitted_at,
        lifecycle,
        observed_effects: vec![],
        configuration: None,
        configuration_observations: vec![],
        trigger_observation: None,
        trigger_observation_201: None,
        composite_schedule_16: None,
        device_model_201: None,
        charging_profile_16: None,
        charging_profile_201: None,
        configuration_201: None,
        local_authorization_16: None,
        local_authorization_201: None,
        reservation_16: None,
        reservation_201: None,
        composite_schedule_201: None,
        charging_profiles_201: None,
        firmware_16: None,
    }
}
fn evidence(limit: ExactDecimal) -> CompositeScheduleResult16 {
    CompositeScheduleResult16 {
        request: CompositeScheduleRequest16 {
            connector_id: 0,
            duration: 60,
            charging_rate_unit: None,
        },
        status: CompositeScheduleStatus16::Accepted,
        connector_id: None,
        schedule_start: Some(at("2026-09-01T14:00:00Z")),
        charging_schedule: Some(CompositeSchedule16 {
            duration: None,
            start_schedule: None,
            charging_rate_unit: CompositeScheduleRateUnit16::W,
            min_charging_rate: Some(ExactDecimal::new(81, 1)),
            charging_schedule_period: vec![CompositeSchedulePeriod16 {
                start_period: 0,
                limit,
                number_phases: Some(4),
            }],
        }),
    }
}
async fn write(store: &Store, result: CommandResult) {
    let mut write = AtomicStoreWrite::empty();
    write.command_result = Some(result);
    store.write_atomic(write).await.unwrap();
}
async fn read(store: &Store, command: &Command<Value>) -> CommandResult {
    store
        .command_result_by_request_id(command.request_id.clone())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Observe terminal evidence across independent writers and reopening.
async fn winning_schedule_survives_stale_conflicting_writers_effect_merging_and_reopen() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "winning-schedule").await;
    let mut winner = result(
        &command,
        CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None,
        },
    );
    winner.recorded_at = at("2026-09-01T14:00:10Z");
    winner.schema_version = ContractVersion::V1_COMPOSITE_SCHEDULE_16;
    winner.composite_schedule_16 = Some(evidence(ExactDecimal::new(9_007_199_254_740_991, 1)));
    write(&store, winner.clone()).await;
    write(&store, result(&command, CommandLifecycle::Dispatched)).await;
    assert_eq!(read(&store, &command).await, winner);
    let other_store = database.open();
    let mut conflicting = result(
        &command,
        CommandLifecycle::ProtocolResponse {
            accepted: false,
            error: Some(CommandError {
                code: CommandErrorCode::ProtocolRejected,
                detail: None,
            }),
        },
    );
    conflicting.recorded_at = at("2026-09-01T15:00:00Z");
    conflicting.composite_schedule_16 = Some(evidence(ExactDecimal::new(0, 0)));
    let effect = ObservedCommandEffect {
        event_id: EventId::new("independent-status").unwrap(),
        event_type: EventType::new("resource.status_changed").unwrap(),
        observed_at: at("2026-09-01T14:00:20Z"),
    };
    conflicting.observed_effects.push(effect.clone());
    write(&other_store, conflicting).await;
    winner.observed_effects.push(effect);
    assert_eq!(read(&store, &command).await, winner);
    let mut wrong_identity = winner.clone();
    wrong_identity.resource.station_id = StationId::new("foreign-station").unwrap();
    let mut invalid = AtomicStoreWrite::empty();
    invalid.command_result = Some(wrong_identity);
    assert!(other_store.write_atomic(invalid).await.is_err());
    assert_eq!(read(&store, &command).await, winner);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    other_store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    assert_eq!(read(&reopened, &command).await, winner);
    let encoded = serde_json::to_value(read(&reopened, &command).await).unwrap();
    assert_eq!(
        encoded["composite_schedule_16"]["charging_schedule"]["charging_schedule_period"][0]["limit"],
        "900719925474099.1"
    );
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn late_schedule_cannot_resolve_terminal_uncertainty_and_old_json_stays_readable() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "uncertain-schedule").await;
    let uncertain = result(
        &command,
        CommandLifecycle::TransmissionUncertain {
            detail: "socket disconnected".to_owned(),
        },
    );
    write(&store, uncertain.clone()).await;
    let mut late = result(
        &command,
        CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None,
        },
    );
    late.schema_version = ContractVersion::V1_COMPOSITE_SCHEDULE_16;
    late.composite_schedule_16 = Some(evidence(ExactDecimal::new(0, 0)));
    write(&store, late).await;
    assert_eq!(read(&store, &command).await, uncertain);
    // Persist a genuinely old payload, with the additive field absent altogether.
    let mut legacy = serde_json::to_value(&uncertain).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("composite_schedule_16");
    rusqlite::Connection::open(&database.0)
        .unwrap()
        .execute(
            "UPDATE command_results SET payload = ?1 WHERE request_id = ?2",
            rusqlite::params![legacy.to_string(), command.request_id.as_str()],
        )
        .unwrap();
    assert_eq!(read(&store, &command).await, uncertain);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    assert_eq!(read(&reopened, &command).await, uncertain);
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}
