use super::*;

pub(super) type Store = SqliteOperationalStore<String, StationEvent, (), ()>;

pub(super) struct Database(pub(super) PathBuf);
impl Database {
    pub(super) fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-trigger-{}.db", Uuid::new_v4())))
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
    UtcTimestamp::new(OffsetDateTime::from_unix_timestamp(1_800_000_000 + seconds).unwrap())
}
pub(super) fn station() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge").unwrap(),
        station_id: StationId::new("charger").unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}
pub(super) fn command() -> Command<String> {
    Command {
        schema_version: ContractVersion::V1_INITIAL,
        request_id: RequestId::new("trigger-one").unwrap(),
        correlation_id: None,
        resource: station(),
        operation: CommandOperation::Ocpp(PrivilegedOcppOperation {
            protocol: ProtocolEdition::Ocpp16j,
            action: ProtocolActionName::new("TriggerMessage").unwrap(),
            payload_schema: PayloadSchemaId::new("trigger-message-16").unwrap(),
            payload: "status-all".to_owned(),
        }),
        expires_at: at(120),
        origin: AuthenticatedCommandOrigin::Management {
            principal_id: PrincipalId::new("operator").unwrap(),
        },
        admitted_at: at(0),
    }
}
pub(super) fn result(response: Option<TriggerNativeResponse>) -> CommandResult {
    let command = command();
    CommandResult {
        schema_version: ContractVersion::V1_TRIGGER,
        correlation_id: None,
        resource: station(),
        return_route: command.return_route(),
        lifecycle: if response.is_some() {
            CommandLifecycle::ProtocolResponse {
                accepted: response == Some(TriggerNativeResponse::Accepted),
                error: None,
            }
        } else {
            CommandLifecycle::Dispatched
        },
        recorded_at: at(2),
        observed_effects: Vec::new(),
        configuration: None,
        configuration_observations: Vec::new(),
        trigger_observation: Some(TriggerObservation {
            requested_class: TriggerMessageClass::StatusNotification,
            native_scope: None,
            expected_targets: vec![1, 2],
            dispatch_started_at: at(2),
            deadline: at(62),
            native_response: response,
            observed: Vec::new(),
            status: TriggerObservationStatus::Pending,
        }),
        trigger_observation_201: None,
        composite_schedule_16: None,
        device_model_201: None,
        charging_profile_16: None,
        charging_profile_201: None,
        configuration_201: None,
        local_authorization_16: None,
    }
}
pub(super) fn event(
    id: &str,
    sequence: u64,
    connector_id: u32,
    class: TriggerMessageClass,
    observed: i64,
) -> EventEnvelope<StationEvent> {
    EventEnvelope {
        event_id: EventId::new(id).unwrap(),
        schema_version: ContractVersion::V1_TRIGGER,
        runtime: RuntimeIdentity {
            environment: Environment::Demo,
            release_id: ReleaseId::new("test").unwrap(),
            release_digest: ArtifactDigest::new("sha256:test").unwrap(),
            process_instance_id: ProcessInstanceId::new("process").unwrap(),
        },
        resource: station(),
        source_time: None,
        observed_at: at(observed),
        event_type: EventType::new("station.snapshot.invalidated").unwrap(),
        origin: EventOrigin::Station,
        sequence,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StationEvent::TriggerNotification {
            station_snapshot_invalidated: StationId::new("charger").unwrap(),
            class,
            connector_id: Some(connector_id),
            status: None,
        },
    }
}
pub(super) async fn put_result(store: &Store, result: CommandResult) {
    let mut write = AtomicStoreWrite::<String, StationEvent, (), ()>::empty();
    write.command_result = Some(result);
    store.write_atomic(write).await.unwrap();
}
pub(super) async fn put_event(store: &Store, event: EventEnvelope<StationEvent>) {
    let mut write = AtomicStoreWrite::<String, StationEvent, (), ()>::empty();
    write.journal_events.push(event);
    store.write_atomic(write).await.unwrap();
}
pub(super) async fn admitted(store: &Store) {
    let mut write = AtomicStoreWrite::<String, StationEvent, (), ()>::empty();
    write.command = Some(command());
    write.command_result = Some(result(None));
    store.write_atomic(write).await.unwrap();
}
pub(super) fn state(result: &CommandResult) -> TriggerObservationStatus {
    result.trigger_observation.as_ref().unwrap().status
}
