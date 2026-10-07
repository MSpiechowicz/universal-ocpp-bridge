use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use uob_application::{
    AtomicStoreWrite, ChargingProfileReportStore201, DeviceModelStore201, OperationalStore,
};
use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;

type Store = SqliteOperationalStore<Value, String, String, String>;
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-schedules201-{}.db", uuid::Uuid::new_v4())))
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
const ACCEPTED: CommandLifecycle = CommandLifecycle::ProtocolResponse {
    accepted: true,
    error: None,
};

async fn admit(store: &Store, id: &str, action: &str, payload: Value) -> Command<Value> {
    let mut command: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    command.request_id = RequestId::new(id).unwrap();
    command.operation = CommandOperation::Ocpp(PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new(action).unwrap(),
        payload_schema: PayloadSchemaId::new(format!("urn:OCPP:Cp:2:2020:3:{action}Request"))
            .unwrap(),
        payload,
    });
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command.clone());
    write.command_result = Some(result(&command, CommandLifecycle::Dispatched));
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
        firmware_201: None,
    }
}
fn profiles(report: ChargingProfileReportState201) -> ChargingProfilesResult201 {
    ChargingProfilesResult201 {
        query: ChargingProfilesQuery201 {
            request_id: 4,
            evse_id: None,
            charging_profile_purpose: None,
            stack_level: Some(0),
            charging_profile_id: vec![],
            charging_limit_source: vec![],
        },
        status: ChargingProfilesStatus201::Accepted,
        reason_code: None,
        report,
    }
}
fn complete(profile_count: usize) -> ChargingProfileReportState201 {
    let profile = ReportedChargingProfile201 {
        evse_id: 0,
        charging_limit_source: ChargingLimitSource201::Cso,
        id: 1,
        stack_level: 0,
        charging_profile_purpose: ReportedChargingProfilePurpose201::ChargingStationMaxProfile,
        charging_profile_kind: ChargingProfileKind201::Absolute,
        transaction_id: None,
        recurrency_kind: None,
        valid_from: None,
        valid_to: None,
        charging_schedule: vec![ChargingSchedule201 {
            id: 1,
            duration: None,
            start_schedule: Some(at("2026-09-01T00:00:00Z")),
            charging_rate_unit: ChargingScheduleRateUnit201::A,
            charging_schedule_period: vec![
                ChargingSchedulePeriod201 {
                    start_period: 0,
                    limit: ExactDecimal::new(9_007_199_254_740_991, 1),
                    number_phases: None,
                    phase_to_use: None,
                };
                1024
            ],
            min_charging_rate: None,
        }],
        sales_tariff_omitted: false,
    };
    ChargingProfileReportState201::Complete {
        progress: ChargingProfileReportProgress201 {
            fragments: 1,
            profiles: profile_count,
            bytes: 1,
        },
        fragments: vec![],
        profiles: vec![profile; profile_count],
    }
}
async fn read(store: &Store, id: &str) -> CommandResult {
    store
        .command_result_by_request_id(RequestId::new(id).unwrap())
        .await
        .unwrap()
        .unwrap()
}
async fn write(store: &Store, value: CommandResult) -> Result<(), uob_application::StorageError> {
    let mut write = AtomicStoreWrite::empty();
    write.command_result = Some(value);
    store.write_atomic(write).await.map(|_| ())
}

#[tokio::test]
async fn acknowledgement_then_report_freezes_terminal_evidence_across_writers_and_reopen() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "report", "GetChargingProfiles", json!({})).await;
    let request = command.request_id.clone();
    // A report cannot precede its durable native acknowledgement.
    assert!(
        store
            .finish_charging_profiles(
                request.clone(),
                profiles(complete(1)),
                None,
                command.admitted_at
            )
            .await
            .is_err()
    );
    let pending = profiles(ChargingProfileReportState201::Pending);
    let acknowledged = store
        .finish_charging_profiles(
            request.clone(),
            pending.clone(),
            Some(ACCEPTED),
            command.admitted_at,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(acknowledged.lifecycle, ACCEPTED);
    assert_eq!(
        acknowledged.schema_version,
        ContractVersion::V1_SCHEDULES_201
    );
    let reported = store
        .finish_charging_profiles(
            request.clone(),
            profiles(complete(1)),
            None,
            command.admitted_at,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reported.charging_profiles_201, Some(profiles(complete(1))));
    // The coordinator's later copy of the acknowledgement cannot reopen the report.
    let mut stale = result(&command, ACCEPTED);
    stale.schema_version = ContractVersion::V1_SCHEDULES_201;
    stale.charging_profiles_201 = Some(pending.clone());
    write(&store, stale).await.unwrap();
    // A writer without the evidence (for example an effect merge) keeps it as well.
    write(&store, result(&command, CommandLifecycle::Dispatched))
        .await
        .unwrap();
    assert_eq!(read(&store, "report").await, reported);
    // A conflicting native status is an identity change, never a merge.
    let mut conflicting = pending;
    conflicting.status = ChargingProfilesStatus201::NoProfiles;
    conflicting.report = ChargingProfileReportState201::NotExpected;
    assert!(
        store
            .finish_charging_profiles(request, conflicting, None, command.admitted_at)
            .await
            .is_err()
    );
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    reopened.interrupt_device_reports().await.unwrap();
    assert_eq!(read(&reopened, "report").await, reported);
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn startup_interrupts_pending_reports_and_bounds_shape_and_output() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "interrupted", "GetChargingProfiles", json!({})).await;
    store
        .finish_charging_profiles(
            command.request_id.clone(),
            profiles(ChargingProfileReportState201::Pending),
            Some(ACCEPTED),
            command.admitted_at,
        )
        .await
        .unwrap();
    // NoProfiles never carries a report, and Accepted never claims none is expected.
    let invalid = admit(&store, "shape", "GetChargingProfiles", json!({})).await;
    for (status, report) in [
        (
            ChargingProfilesStatus201::NoProfiles,
            ChargingProfileReportState201::Pending,
        ),
        (
            ChargingProfilesStatus201::Accepted,
            ChargingProfileReportState201::NotExpected,
        ),
    ] {
        let mut evidence = profiles(report);
        evidence.status = status;
        assert!(
            store
                .finish_charging_profiles(
                    invalid.request_id.clone(),
                    evidence,
                    Some(ACCEPTED),
                    invalid.admitted_at
                )
                .await
                .is_err()
        );
    }
    let large = admit(&store, "large", "GetChargingProfiles", json!({})).await;
    store
        .finish_charging_profiles(
            large.request_id.clone(),
            profiles(ChargingProfileReportState201::Pending),
            Some(ACCEPTED),
            large.admitted_at,
        )
        .await
        .unwrap();
    let bounded = store
        .finish_charging_profiles(
            large.request_id.clone(),
            profiles(complete(64)),
            None,
            large.admitted_at,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        bounded.charging_profiles_201.unwrap().report,
        ChargingProfileReportState201::Incomplete {
            reason: ChargingProfileReportFailure201::OutputLimit,
            progress: Some(ChargingProfileReportProgress201 { profiles: 64, .. })
        }
    ));
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    reopened.interrupt_device_reports().await.unwrap();
    let interrupted = read(&reopened, "interrupted").await;
    assert_eq!(interrupted.lifecycle, ACCEPTED);
    assert_eq!(
        interrupted.charging_profiles_201.unwrap().report,
        ChargingProfileReportState201::Incomplete {
            reason: ChargingProfileReportFailure201::Interrupted,
            progress: None
        }
    );
    // An interrupted report is terminal; a late collector cannot complete it.
    assert_eq!(
        reopened
            .finish_charging_profiles(
                command.request_id,
                profiles(complete(1)),
                None,
                command.admitted_at
            )
            .await
            .unwrap()
            .unwrap()
            .charging_profiles_201
            .unwrap()
            .report,
        ChargingProfileReportState201::Incomplete {
            reason: ChargingProfileReportFailure201::Interrupted,
            progress: None
        }
    );
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn terminal_composite_schedule_survives_stale_writers_and_old_json_reads() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "schedule", "GetCompositeSchedule", json!({})).await;
    let mut winner = result(&command, ACCEPTED);
    winner.schema_version = ContractVersion::V1_SCHEDULES_201;
    winner.composite_schedule_201 = Some(CompositeScheduleResult201 {
        request: CompositeScheduleRequest201 {
            evse_id: 0,
            duration: 60,
            charging_rate_unit: None,
        },
        status: CompositeScheduleStatus201::Accepted,
        reason_code: Some(SmartChargingReason201::InternalError),
        schedule: Some(CompositeSchedule201 {
            evse_id: 0,
            duration: 60,
            schedule_start: at("2026-09-01T00:00:00Z"),
            charging_rate_unit: ChargingScheduleRateUnit201::W,
            charging_schedule_period: vec![ChargingSchedulePeriod201 {
                start_period: 0,
                limit: ExactDecimal::new(0, 0),
                number_phases: Some(1),
                phase_to_use: Some(3),
            }],
        }),
    });
    write(&store, winner.clone()).await.unwrap();
    let mut stale = result(
        &command,
        CommandLifecycle::TransmissionUncertain {
            detail: "late writer".to_owned(),
        },
    );
    stale.composite_schedule_201 = None;
    write(&store, stale).await.unwrap();
    assert_eq!(read(&store, "schedule").await, winner);
    // Results recorded before this revision decode with both new fields absent.
    let mut old = serde_json::to_value(result(&command, ACCEPTED)).unwrap();
    old.as_object_mut()
        .unwrap()
        .retain(|key, _| !key.ends_with("_201"));
    let decoded: CommandResult = serde_json::from_value(old).unwrap();
    assert!(decoded.composite_schedule_201.is_none() && decoded.charging_profiles_201.is_none());
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
