use super::*;

pub(super) type Store = SqliteOperationalStore<String, StationEvent, (), ()>;

pub(super) struct Database(pub(super) PathBuf);
impl Database {
    pub(super) fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-trigger-201-{}.db", Uuid::new_v4())))
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

pub(super) fn station(id: &str) -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge").unwrap(),
        station_id: StationId::new(id).unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}

pub(super) fn command(
    id: &str,
    class: TriggerMessageClass201,
    protocol: ProtocolEdition,
) -> Command<String> {
    Command {
        schema_version: ContractVersion::V1_INITIAL,
        request_id: RequestId::new(id).unwrap(),
        correlation_id: None,
        resource: station("charger"),
        operation: CommandOperation::Ocpp(PrivilegedOcppOperation {
            protocol,
            action: ProtocolActionName::new("TriggerMessage").unwrap(),
            payload_schema: PayloadSchemaId::new("trigger-message-201").unwrap(),
            payload: format!("{class:?}"),
        }),
        expires_at: at(120),
        origin: AuthenticatedCommandOrigin::Management {
            principal_id: PrincipalId::new("operator").unwrap(),
        },
        admitted_at: at(0),
    }
}

pub(super) fn result(
    command: &Command<String>,
    class: TriggerMessageClass201,
    targets: Vec<TriggerTarget201>,
    scope: Option<TriggerEvse201>,
    response: Option<TriggerNativeStatus201>,
) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_TRIGGER_201,
        correlation_id: None,
        resource: command.resource.clone(),
        return_route: command.return_route(),
        lifecycle: if response.is_some() {
            CommandLifecycle::ProtocolResponse {
                accepted: response == Some(TriggerNativeStatus201::Accepted),
                error: None,
            }
        } else {
            CommandLifecycle::Dispatched
        },
        recorded_at: at(2),
        observed_effects: Vec::new(),
        configuration: None,
        configuration_observations: Vec::new(),
        trigger_observation: None,
        composite_schedule_16: None,
        trigger_observation_201: Some(TriggerObservation201 {
            requested_class: class,
            native_scope: scope,
            expected_targets: targets,
            dispatch_started_at: at(2),
            deadline: at(62),
            native_response: response.map(|status| TriggerNativeResponse201 {
                status,
                status_info: None,
            }),
            observed: Vec::new(),
            status: TriggerObservationStatus201::Pending,
        }),
        device_model_201: None,
        charging_profile_16: None,
        charging_profile_201: None,
        configuration_201: None,
        local_authorization_16: None,
        local_authorization_201: None,
    }
}

pub(super) fn marker(
    id: &str,
    sequence: u64,
    station_id: &str,
    class: TriggerMessageClass201,
    target: TriggerTarget201,
    when: UtcTimestamp,
    status: Option<&str>,
) -> EventEnvelope<StationEvent> {
    EventEnvelope {
        event_id: EventId::new(id).unwrap(),
        schema_version: ContractVersion::V1_TRIGGER_201,
        runtime: RuntimeIdentity {
            environment: Environment::Demo,
            release_id: ReleaseId::new("test").unwrap(),
            release_digest: ArtifactDigest::new("sha256:test").unwrap(),
            process_instance_id: ProcessInstanceId::new("process").unwrap(),
        },
        resource: station(station_id),
        source_time: None,
        observed_at: when,
        event_type: EventType::new("station.snapshot.invalidated").unwrap(),
        origin: EventOrigin::Station,
        sequence,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StationEvent::TriggerNotification201 {
            station_snapshot_invalidated: StationId::new(station_id).unwrap(),
            trigger_class_201: class,
            target,
            status: status.map(str::to_owned),
        },
    }
}

pub(super) async fn admit(store: &Store, command: Command<String>, result: CommandResult) {
    let mut write = AtomicStoreWrite::<String, StationEvent, (), ()>::empty();
    write.command = Some(command);
    write.command_result = Some(result);
    store.write_atomic(write).await.unwrap();
}

pub(super) async fn put_result(store: &Store, result: CommandResult) {
    let mut write = AtomicStoreWrite::<String, StationEvent, (), ()>::empty();
    write.command_result = Some(result);
    store.write_atomic(write).await.unwrap();
}

pub(super) async fn put_marker(store: &Store, event: EventEnvelope<StationEvent>) {
    let mut write = AtomicStoreWrite::<String, StationEvent, (), ()>::empty();
    write.journal_events.push(event);
    store.write_atomic(write).await.unwrap();
}

pub(super) async fn reconcile(store: &Store, id: &str, when: i64) -> CommandResult {
    store
        .reconcile_trigger_observation(RequestId::new(id).unwrap(), at(when))
        .await
        .unwrap()
        .unwrap()
}

pub(super) fn observation(result: &CommandResult) -> &TriggerObservation201 {
    result.trigger_observation_201.as_ref().unwrap()
}
