use serde::{Deserialize, Serialize};
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_contracts::{
    ArtifactDigest, BridgeId, CanonicalConnectorId, CanonicalEvseId, CanonicalResource,
    ContractVersion, Environment, EventEnvelope, EventId, EventOrigin, EventType,
    NativeProtocolReference, ProcessInstanceId, ReleaseId, ResourceRef, RuntimeIdentity, StationId,
    UtcTimestamp,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StatusPayload {
    status: String,
}

fn id<T, E: std::fmt::Debug>(constructor: impl FnOnce(String) -> Result<T, E>, value: &str) -> T {
    constructor(value.to_owned()).expect("valid fixture identity")
}

fn timestamp(hour: u8, offset: UtcOffset) -> UtcTimestamp {
    let local = PrimitiveDateTime::new(
        Date::from_calendar_date(2026, Month::September, 1).expect("fixture date"),
        Time::from_hms(hour, 30, 0).expect("fixture time"),
    )
    .assume_offset(offset);
    UtcTimestamp::new(local)
}

fn runtime(environment: Environment, release: &str, process: &str) -> RuntimeIdentity {
    RuntimeIdentity {
        environment,
        release_id: id(ReleaseId::new, release),
        release_digest: id(ArtifactDigest::new, format!("sha256:{release}").as_str()),
        process_instance_id: id(ProcessInstanceId::new, process),
    }
}

fn resource() -> ResourceRef {
    ResourceRef {
        bridge_id: id(BridgeId::new, "bridge-berlin-1"),
        station_id: id(StationId::new, "station-7"),
        resource: Some(CanonicalResource::Evse {
            evse_id: id(CanonicalEvseId::new, "evse-a"),
            connector_id: Some(id(CanonicalConnectorId::new, "cable-right")),
        }),
        native_protocol_reference: Some(NativeProtocolReference::Ocpp201 {
            evse_id: 1,
            connector_id: Some(2),
        }),
    }
}

fn event(event_id: &str, runtime: RuntimeIdentity) -> EventEnvelope<StatusPayload> {
    EventEnvelope {
        event_id: id(EventId::new, event_id),
        schema_version: ContractVersion::V1_INITIAL,
        runtime,
        resource: resource(),
        source_time: None,
        observed_at: timestamp(12, UtcOffset::from_hms(2, 0, 0).expect("fixture offset")),
        event_type: id(EventType::new, "station.status.changed.v1"),
        origin: EventOrigin::Station,
        sequence: 42,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StatusPayload {
            status: "available".to_owned(),
        },
    }
}

#[test]
fn fixture_preserves_evse_connector_distinction_and_absent_source_time() {
    let value = serde_json::to_value(event(
        "evt-production-42",
        runtime(Environment::Production, "release-a", "process-a"),
    ))
    .expect("serialize fixture");

    assert_eq!(
        value,
        serde_json::from_str::<serde_json::Value>(include_str!("fixtures/event-envelope-v1.json"))
            .expect("parse checked-in fixture")
    );
    assert!(value.get("source_time").is_none());
    assert_eq!(value["resource"]["resource"]["kind"], "evse");
    assert_eq!(
        value["resource"]["native_protocol_reference"]["protocol"],
        "ocpp201"
    );
    assert_eq!(value["observed_at"], "2026-09-01T10:30:00Z");
}

#[test]
fn runtime_changes_do_not_rename_logical_resources() {
    let first = event(
        "evt-a",
        runtime(Environment::Production, "release-a", "process-a"),
    );
    let second = event(
        "evt-b",
        runtime(Environment::Production, "release-b", "process-b"),
    );

    assert_eq!(first.resource, second.resource);
    assert_ne!(first.runtime, second.runtime);
}

#[test]
fn replay_requires_a_new_identity_and_retains_original_only_as_provenance() {
    let mut original = event(
        "evt-production-42",
        runtime(Environment::Production, "release-a", "process-a"),
    );
    original.source_time = Some(timestamp(9, UtcOffset::UTC));
    let collision = original.clone().into_staging_replay(
        id(EventId::new, "evt-production-42"),
        runtime(Environment::Staging, "release-a", "process-staging"),
        timestamp(11, UtcOffset::UTC),
    );
    assert!(collision.is_err());

    let replay = original
        .into_staging_replay(
            id(EventId::new, "evt-staging-99"),
            runtime(Environment::Staging, "release-a", "process-staging"),
            timestamp(11, UtcOffset::UTC),
        )
        .expect("isolated staging replay");

    assert_eq!(replay.event_id.as_str(), "evt-staging-99");
    assert_eq!(replay.origin, EventOrigin::Replay);
    assert_eq!(replay.source_time, Some(timestamp(9, UtcOffset::UTC)));
    assert_eq!(
        replay
            .provenance
            .expect("original provenance")
            .original_event_id
            .as_str(),
        "evt-production-42"
    );
}

#[test]
fn connector_and_evse_are_different_canonical_variants() {
    let connector = CanonicalResource::Connector {
        connector_id: id(CanonicalConnectorId::new, "1"),
    };
    let evse = CanonicalResource::Evse {
        evse_id: id(CanonicalEvseId::new, "1"),
        connector_id: None,
    };

    assert_ne!(
        serde_json::to_value(connector).expect("connector JSON"),
        serde_json::to_value(evse).expect("EVSE JSON")
    );
}

#[test]
fn deserialized_timestamps_are_normalized_before_reemission() {
    let timestamp: UtcTimestamp =
        serde_json::from_str("\"2026-09-01T12:30:00+02:00\"").expect("timestamp input");

    assert_eq!(
        serde_json::to_string(&timestamp).expect("timestamp output"),
        "\"2026-09-01T10:30:00Z\""
    );
}

#[test]
fn empty_identities_are_rejected_during_deserialization() {
    assert!(serde_json::from_str::<EventId>("\"  \"").is_err());
    assert!(serde_json::from_str::<BridgeId>("\"\"").is_err());
}
