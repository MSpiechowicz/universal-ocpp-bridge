use super::*;
use std::path::PathBuf;
pub(super) type Store = SqliteOperationalStore<Value, StationEvent, (), ()>;
pub(super) struct Database(PathBuf);
impl Database {
    pub(super) fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-device-{}.db", uuid::Uuid::new_v4())))
    }
    pub(super) fn open(&self) -> Store {
        Store::open(&self.0, 16).unwrap()
    }
    pub(super) fn staging_count(&self) -> i64 {
        self.count("SELECT count(*) FROM device_report_staging")
    }
    pub(super) fn pending_count(&self) -> i64 {
        self.count("SELECT count(*) FROM command_results WHERE report_pending = 1")
    }
    fn count(&self, sql: &str) -> i64 {
        rusqlite::Connection::open(&self.0)
            .unwrap()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(format!("{}-wal", self.0.display()));
        let _ = std::fs::remove_file(format!("{}-shm", self.0.display()));
    }
}
pub(super) fn at(seconds: i64) -> UtcTimestamp {
    UtcTimestamp::new(time::OffsetDateTime::from_unix_timestamp(1_800_000_000 + seconds).unwrap())
}
pub(super) fn fixture(
    id: &str,
    ack: Option<DeviceReportAck201>,
) -> (Command<Value>, CommandResult) {
    let command = Command {
        schema_version: ContractVersion::V1_INITIAL,
        request_id: RequestId::new(id).unwrap(),
        correlation_id: None,
        resource: ResourceRef {
            bridge_id: BridgeId::new("bridge").unwrap(),
            station_id: StationId::new("charger").unwrap(),
            resource: None,
            native_protocol_reference: None,
        },
        operation: CommandOperation::Ocpp(PrivilegedOcppOperation {
            protocol: ProtocolEdition::Ocpp201,
            action: ProtocolActionName::new("GetBaseReport").unwrap(),
            payload_schema: PayloadSchemaId::new("urn:OCPP:Cp:2:2020:3:GetBaseReportRequest")
                .unwrap(),
            payload: json!({"requestId":-1,"reportBase":"FullInventory"}),
        }),
        expires_at: at(120),
        admitted_at: at(0),
        origin: AuthenticatedCommandOrigin::Management {
            principal_id: PrincipalId::new("operator").unwrap(),
        },
    };
    let evidence = DeviceModelResult201 {
        query: DeviceModelQuery201::GetBaseReport {
            request_id: -1,
            report_base: DeviceReportBase201::FullInventory,
        },
        connection: CorrelationId::new("connection").unwrap(),
        generation: 7,
        dispatch_recorded_at: at(1),
        native_ack: ack,
        variables: Vec::new(),
        report: DeviceReportState201::Pending,
    };
    let result = CommandResult {
        schema_version: ContractVersion::V1_DEVICE_MODEL_201,
        correlation_id: None,
        resource: command.resource.clone(),
        return_route: command.return_route(),
        lifecycle: if ack.is_some() {
            CommandLifecycle::ProtocolResponse {
                accepted: true,
                error: None,
            }
        } else {
            CommandLifecycle::Dispatched
        },
        recorded_at: at(1),
        observed_effects: Vec::new(),
        configuration: None,
        configuration_observations: Vec::new(),
        trigger_observation: None,
        trigger_observation_201: None,
        composite_schedule_16: None,
        device_model_201: Some(evidence),
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
    };
    (command, result)
}
pub(super) fn complete(evidence: &DeviceModelResult201) -> DeviceModelResult201 {
    let mut result = evidence.clone();
    result.report = DeviceReportState201::Complete {
        progress: DeviceReportProgress201 {
            fragments: 1,
            items: 1,
            bytes: 100,
        },
        fragments: vec![DeviceReportFragment201 {
            generated_at: "2026-09-01T00:00:00Z".to_owned(),
            sequence: 0,
            more: false,
            items: 1,
        }],
        items: vec![DeviceReportItem201 {
            component: DeviceComponent201 {
                name: "SecurityCtrlr".to_owned(),
                instance: None,
                evse: None,
            },
            variable: DeviceVariable201 {
                name: "PrivateKey".to_owned(),
                instance: None,
            },
            attributes: vec![DeviceReportAttribute201 {
                attribute_type: DeviceAttributeType201::Actual,
                value: DeviceValue201 {
                    present: true,
                    redacted: true,
                    empty: false,
                    value: None,
                },
                mutability: Some(DeviceMutability201::WriteOnly),
                persistent: Some(true),
                constant: None,
            }],
            characteristics: None,
        }],
    };
    result
}
pub(super) async fn admit(store: &Store, command: Command<Value>, result: CommandResult) {
    let mut write = AtomicStoreWrite::<Value, StationEvent, (), ()>::empty();
    write.command = Some(command);
    write.command_result = Some(result);
    store.write_atomic(write).await.unwrap();
}
