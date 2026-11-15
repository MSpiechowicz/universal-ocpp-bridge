use serde_json::{Value, json};
use std::path::PathBuf;
use uob_application::{AtomicStoreWrite, OperationalStore};
use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;

pub type Store = SqliteOperationalStore<Value, StationEvent, String, String>;

pub const ACCEPTED: CommandLifecycle = CommandLifecycle::ProtocolResponse {
    accepted: true,
    error: None,
};

pub struct Database(pub PathBuf);

impl Database {
    pub fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-monitoring201-{}.db", uuid::Uuid::new_v4())))
    }

    pub fn open(&self) -> Store {
        Store::open(&self.0, 16).unwrap()
    }

    pub fn connection(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.0).unwrap()
    }

    pub fn version(&self) -> i64 {
        self.connection()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap()
    }

    pub fn report_pending(&self, request: &str) -> i64 {
        self.connection()
            .query_row(
                "SELECT report_pending FROM command_results WHERE request_id = ?1",
                [request],
                |row| row.get(0),
            )
            .unwrap()
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

pub fn at(value: &str) -> UtcTimestamp {
    serde_json::from_value(json!(value)).unwrap()
}

/// Admits a dispatched command of the given native action.
pub async fn admit(store: &Store, id: &str, action: &str) -> Command<Value> {
    let mut command: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    command.request_id = RequestId::new(id).unwrap();
    command.operation = CommandOperation::Ocpp(PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new(action).unwrap(),
        payload_schema: PayloadSchemaId::new(format!("urn:OCPP:Cp:2:2020:3:{action}Request"))
            .unwrap(),
        payload: json!({}),
    });
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command.clone());
    write.command_result = Some(result(&command, CommandLifecycle::Dispatched));
    store.write_atomic(write).await.unwrap();
    command
}

pub fn result(command: &Command<Value>, lifecycle: CommandLifecycle) -> CommandResult {
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
        diagnostics_16: None,
        diagnostics_201: None,
        monitoring_201: None,
    }
}

/// The result a native reply is recorded as, at the monitoring revision.
pub fn monitoring(command: &Command<Value>, evidence: MonitoringResult201) -> CommandResult {
    let mut value = result(command, ACCEPTED);
    value.schema_version = ContractVersion::V1_MONITORING_201;
    value.monitoring_201 = Some(evidence);
    value
}

pub async fn write(
    store: &Store,
    value: CommandResult,
) -> Result<(), uob_application::StorageError> {
    let mut write = AtomicStoreWrite::empty();
    write.command_result = Some(value);
    store.write_atomic(write).await.map(|_| ())
}

pub async fn read(store: &Store, id: &str) -> CommandResult {
    store
        .command_result_by_request_id(RequestId::new(id).unwrap())
        .await
        .unwrap()
        .unwrap()
}

pub fn component() -> DeviceComponent201 {
    DeviceComponent201 {
        name: "EVSE".to_owned(),
        instance: None,
        evse: Some(DeviceEvse201 {
            id: 1,
            connector_id: Some(2),
        }),
    }
}

pub fn variable() -> DeviceVariable201 {
    DeviceVariable201 {
        name: "Power".to_owned(),
        instance: Some("Import".to_owned()),
    }
}

pub fn set_monitor() -> SetMonitor201 {
    SetMonitor201 {
        id: None,
        transaction: Some(true),
        value: "7400.125".parse().unwrap(),
        monitor_type: MonitorType201::UpperThreshold,
        severity: MonitorSeverity201::new(3).unwrap(),
        component: component(),
        variable: variable(),
    }
}

pub fn set_result(status: SetMonitoringStatus201) -> SetMonitorResult201 {
    SetMonitorResult201 {
        id: status.accepted().then_some(11),
        status,
        reason_code: None,
        monitor_type: MonitorType201::UpperThreshold,
        severity: MonitorSeverity201::new(3).unwrap(),
        component: component(),
        variable: variable(),
    }
}

pub fn clear_result(id: i32) -> ClearMonitorResult201 {
    ClearMonitorResult201 {
        id,
        status: ClearMonitoringStatus201::Accepted,
        reason_code: None,
    }
}

pub fn set_evidence() -> MonitoringResult201 {
    MonitoringResult201 {
        query: MonitoringQuery201::SetVariableMonitoring {
            monitors: vec![set_monitor()],
        },
        outcome: MonitoringOutcome201::SetMonitors {
            monitors: vec![set_result(SetMonitoringStatus201::Accepted)],
        },
    }
}

pub fn clear_evidence() -> MonitoringResult201 {
    MonitoringResult201 {
        query: MonitoringQuery201::ClearVariableMonitoring { ids: vec![4, 5] },
        outcome: MonitoringOutcome201::ClearMonitors {
            monitors: vec![clear_result(5), clear_result(4)],
        },
    }
}

pub fn base_evidence() -> MonitoringResult201 {
    MonitoringResult201 {
        query: MonitoringQuery201::SetMonitoringBase {
            base: MonitoringBase201::FactoryDefault,
        },
        outcome: MonitoringOutcome201::Base {
            status: DeviceReportAck201::Accepted,
        },
    }
}

pub fn level_evidence() -> MonitoringResult201 {
    MonitoringResult201 {
        query: MonitoringQuery201::SetMonitoringLevel {
            severity: MonitorSeverity201::new(6).unwrap(),
        },
        outcome: MonitoringOutcome201::Level {
            status: MonitoringLevelStatus201::Rejected,
        },
    }
}

pub fn report_evidence(report: MonitoringReportState201) -> MonitoringResult201 {
    MonitoringResult201 {
        query: MonitoringQuery201::GetMonitoringReport {
            request_id: -3,
            criteria: vec![MonitoringCriterion201::ThresholdMonitoring],
            component_variables: vec![],
        },
        outcome: MonitoringOutcome201::Report {
            ack: Some(DeviceReportAck201::Accepted),
            report,
        },
    }
}

pub fn reported_monitor(id: i32) -> ReportedMonitor201 {
    ReportedMonitor201 {
        component: component(),
        variable: variable(),
        id,
        transaction: false,
        value: "-0.5".parse().unwrap(),
        monitor_type: MonitorType201::Delta,
        severity: MonitorSeverity201::new(9).unwrap(),
    }
}

pub fn complete(monitors: usize) -> MonitoringReportState201 {
    MonitoringReportState201::Complete {
        fragments: vec![MonitoringReportFragment201 {
            generated_at: at("2026-10-08T10:15:01Z"),
            sequence: 0,
            more: false,
            monitors: u32::try_from(monitors).unwrap(),
        }],
        monitors: (0..monitors)
            .map(|id| reported_monitor(i32::try_from(id).unwrap()))
            .collect(),
        monitors_omitted: 0,
    }
}
