use super::*;

pub(super) fn resource() -> ResourceRef {
    ResourceRef {
        bridge_id: text(BridgeId::new, "bridge-1"),
        station_id: text(StationId::new, "station-1"),
        resource: None,
        native_protocol_reference: None,
    }
}

pub(super) fn command() -> Command<TestCommandPayload> {
    ExternalCommand::authenticated(
        CommandRequest {
            request_id: text(RequestId::new, "request-1"),
            correlation_id: None,
            resource: resource(),
            operation: CommandOperation::Start {
                authorization_reference: None,
            },
            expires_at: timestamp(9),
        },
        AuthenticatedCommandOrigin::Management {
            principal_id: text(PrincipalId::new, "operator-1"),
        },
    )
    .admit(timestamp(0))
}

fn snapshot() -> StationSnapshot {
    StationSnapshot {
        schema_version: ContractVersion::V1_INITIAL,
        station: resource(),
        observed_at: timestamp(0),
        connectivity: Connectivity::Connected {
            protocol: uob_contracts::ProtocolEdition::Ocpp16j,
            connected_at: timestamp(0),
            last_message_at: Some(timestamp(0)),
        },
        capabilities: ResourceCapabilities::default(),
        resources: Vec::new(),
        transactions: Vec::new(),
        current_values: Vec::new(),
    }
}

fn event() -> EventEnvelope<TestEventPayload> {
    EventEnvelope {
        event_id: text(EventId::new, "event-1"),
        schema_version: ContractVersion::V1_INITIAL,
        runtime: RuntimeIdentity {
            environment: uob_contracts::Environment::Demo,
            release_id: text(ReleaseId::new, "release-1"),
            release_digest: text(uob_contracts::ArtifactDigest::new, "sha256:abc"),
            process_instance_id: text(ProcessInstanceId::new, "process-1"),
        },
        resource: resource(),
        source_time: None,
        observed_at: timestamp(0),
        event_type: text(EventType::new, "command.admitted.v1"),
        origin: EventOrigin::Management,
        sequence: 1,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: "admitted".to_owned(),
    }
}

pub(super) fn populated_write()
-> AtomicStoreWrite<TestCommandPayload, TestEventPayload, TestDeliveryPayload, TestCommittedPayload>
{
    AtomicStoreWrite {
        charging_profile_201: None,
        reservation_16: None,
        reservation_observations_16: Vec::new(),
        reservation_201: None,
        reservation_observations_201: Vec::new(),
        purpose: uob_application::StorageWritePurpose::Routine,
        station_snapshot: Some(snapshot()),
        authorization_changes: vec![AuthorizationChange {
            reference: text(AuthorizationReference::new, "local-auth-1"),
            resource: resource(),
            state: AuthorizationState::Active,
            revision: 1,
            changed_at: timestamp(0),
            expires_at: None,
        }],
        command: Some(command()),
        command_result: None,
        journal_events: vec![event()],
        required_deliveries: vec![PendingDelivery {
            delivery_id: text(DeliveryId::new, "delivery-1"),
            event_id: text(EventId::new, "event-1"),
            target_instance_id: text(TargetInstanceId::new, "target-main"),
            target_configuration_revision: 4,
            ordering_key: resource(),
            deadline: timestamp(8),
            durability: Durability::Critical,
            payload: "event delivery".to_owned(),
        }],
        committed_records: vec![
            CommittedRecord {
                record_id: text(CommittedRecordId::new, "record-1"),
                durability: Durability::Critical,
                committed_at: timestamp(0),
                record: "export event".to_owned(),
            },
            CommittedRecord {
                record_id: text(CommittedRecordId::new, "record-telemetry-1"),
                durability: Durability::BestEffortTelemetry,
                committed_at: timestamp(0),
                record: "export telemetry".to_owned(),
            },
        ],
    }
}
