#![cfg(target_os = "linux")]
#[path = "export_ingestion/oversized.rs"]
mod oversized;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
};

use time::{Date, Duration, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_application::{
    AtomicStoreWrite, CommittedRecord, CommittedRecordField, CommittedRecordId, Durability,
    EXPORT_RECORD_CHUNK_BYTES, ExportGapReason, ExportIngestor, ExportPendingDescriptor,
    ExportSpool, ExportSpoolErrorCode, ExportSpoolNamespace, OPERATIONAL_HISTORY_RETENTION_SECONDS,
    OperationalStore, PageLimit, RuntimeResourceBudget, RuntimeResourceLimits, StorageWritePurpose,
};
use uob_contracts::{
    ArtifactDigest, AvailabilityState, BridgeId, Environment, ExportDestination,
    ExportDestinationId, ExportPayload, ExportRecord, ExportRecordId, ExportRecordIdentity,
    ExportRecordMetadata, ExportResourceStatusChange, ProcessInstanceId, ReleaseId, ResourceRef,
    RuntimeIdentity, StationId, UtcTimestamp,
};
use uob_external_export_adapter::ExportBacklogState;
use uob_storage_adapter::{ExportSpoolLimits, SqliteExportSpool, SqliteOperationalStore};
use uuid::Uuid;

type Source = SqliteOperationalStore<String, String, String, ExportRecord>;

struct Fixture {
    source_path: PathBuf,
    spool_path: PathBuf,
}
impl Fixture {
    fn new() -> Option<Self> {
        let operational = std::env::temp_dir();
        let isolated = PathBuf::from("/dev/shm");
        if fs::metadata(&operational).ok()?.dev() == fs::metadata(&isolated).ok()?.dev() {
            return None;
        }
        let unique = Uuid::new_v4();
        let spool_path = isolated.join(format!("uob-export-{unique}"));
        fs::create_dir(&spool_path).unwrap();
        fs::set_permissions(&spool_path, fs::Permissions::from_mode(0o700)).unwrap();
        Some(Self {
            source_path: operational.join(format!("uob-export-source-{unique}.sqlite3")),
            spool_path,
        })
    }
    fn open(&self) -> (Source, SqliteExportSpool) {
        self.open_with_size(1024 * 1024)
    }

    fn open_with_size(&self, main_bytes: u64) -> (Source, SqliteExportSpool) {
        (
            Source::open(&self.source_path, 8).unwrap(),
            SqliteExportSpool::open_with_limits(
                &self.spool_path,
                &self.source_path,
                8,
                ExportSpoolLimits::new(main_bytes, 8).unwrap(),
                &budget(),
            )
            .unwrap(),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.source_path);
        let _ = fs::remove_file(format!("{}-wal", self.source_path.display()));
        let _ = fs::remove_file(format!("{}-shm", self.source_path.display()));
        let _ = fs::remove_file(self.spool_path.join("export.sqlite3"));
        let _ = fs::remove_dir(&self.spool_path);
    }
}
fn instant() -> UtcTimestamp {
    UtcTimestamp::new(
        PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::September, 1).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        )
        .assume_offset(UtcOffset::UTC),
    )
}
fn record(id: &str, durability: Durability) -> CommittedRecord<ExportRecord> {
    let observed_at = instant();
    CommittedRecord {
        record_id: CommittedRecordId::new(id).unwrap(),
        durability,
        committed_at: observed_at,
        record: ExportRecord::new(
            ExportRecordMetadata {
                identity: ExportRecordIdentity::root(ExportRecordId::new(id).unwrap()),
                schema_version: ExportRecord::SCHEMA_VERSION,
                runtime: RuntimeIdentity {
                    environment: Environment::Demo,
                    release_id: ReleaseId::new("test-release").unwrap(),
                    release_digest: ArtifactDigest::new("sha256:test").unwrap(),
                    process_instance_id: ProcessInstanceId::new("test-process").unwrap(),
                },
                resource: ResourceRef {
                    bridge_id: BridgeId::new("test-bridge").unwrap(),
                    station_id: StationId::new("station-1").unwrap(),
                    resource: None,
                    native_protocol_reference: None,
                },
                source_time: None,
                observed_at,
                sequence: 1,
                correlation_id: None,
            },
            ExportPayload::ResourceStatusChange(ExportResourceStatusChange {
                previous: AvailabilityState::Unknown,
                current: AvailabilityState::Available,
            }),
        ),
    }
}
fn destination() -> ExportDestination {
    ExportDestination {
        destination_id: ExportDestinationId::new("analytics").unwrap(),
        configuration_revision: 7,
    }
}
async fn commit(source: &Source, records: Vec<CommittedRecord<ExportRecord>>) {
    source
        .write_atomic(AtomicStoreWrite {
            charging_profile_201: None,
            reservation_16: None,
            reservation_observations_16: Vec::new(),
            reservation_201: None,
            reservation_observations_201: Vec::new(),
            firmware_16: None,
            firmware_observations_16: Vec::new(),
            firmware_201: None,
            diagnostics_16: None,
            diagnostics_201: None,
            firmware_observations_201: Vec::new(),
            diagnostics_observations_16: Vec::new(),
            diagnostics_observations_201: Vec::new(),
            purpose: StorageWritePurpose::Routine,
            station_snapshot: None,
            authorization_changes: vec![],
            command: None,
            command_result: None,
            journal_events: vec![],
            required_deliveries: vec![],
            committed_records: records,
        })
        .await
        .unwrap();
}
fn budget() -> RuntimeResourceBudget {
    RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap()
}
async fn pending_field(
    spool: &SqliteExportSpool,
    namespace: &ExportSpoolNamespace,
    descriptor: &ExportPendingDescriptor,
    field: CommittedRecordField,
) -> Vec<u8> {
    let length = match field {
        CommittedRecordField::RecordId => descriptor.record_id_len,
        CommittedRecordField::CommittedAt => descriptor.committed_at_len,
        CommittedRecordField::Payload => descriptor.payload_len,
    };
    let mut bytes = Vec::new();
    let mut offset = 0;
    while offset < length {
        let chunk = spool
            .pending_chunk(
                namespace.clone(),
                descriptor.clone(),
                field,
                offset,
                EXPORT_RECORD_CHUNK_BYTES,
            )
            .await
            .unwrap();
        assert!(chunk.next_offset > offset);
        offset = chunk.next_offset;
        bytes.extend_from_slice(&chunk.bytes);
    }
    bytes
}

async fn pending_ids(spool: &SqliteExportSpool, namespace: &ExportSpoolNamespace) -> Vec<String> {
    let mut after = None;
    let mut ids = Vec::new();
    loop {
        let page = spool
            .pending(namespace.clone(), after, PageLimit::new(100).unwrap())
            .await
            .unwrap();
        for descriptor in &page.items {
            ids.push(
                String::from_utf8(
                    pending_field(spool, namespace, descriptor, CommittedRecordField::RecordId)
                        .await,
                )
                .unwrap(),
            );
        }
        if !page.has_more {
            break;
        }
        after = page.resume;
    }
    ids
}

#[tokio::test]
async fn real_source_ingests_and_recovers_without_relabeling_pending_work() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let (source, spool) = fixture.open();
    commit(
        &source,
        vec![
            record("critical-1", Durability::Critical),
            record("telemetry-1", Durability::BestEffortTelemetry),
        ],
    )
    .await;
    let first =
        ExportIngestor::ingest_once(&source, &spool, destination(), "postgresql", &budget())
            .await
            .unwrap();
    assert_eq!(first.status.pending_records, 2);
    assert_eq!(first.status.critical.as_ref().unwrap().sequence, 1);
    assert_eq!(first.status.telemetry.as_ref().unwrap().sequence, 1);
    let again =
        ExportIngestor::ingest_once(&source, &spool, destination(), "postgresql", &budget())
            .await
            .unwrap();
    assert_eq!(again.status.pending_records, 2);
    let namespace = uob_application::ExportSpoolNamespace {
        destination: destination(),
        provider_kind: "postgresql".into(),
        source_generation: first.status.source_generation.clone(),
    };
    drop(spool);
    let reopened = SqliteExportSpool::open_with_limits(
        &fixture.spool_path,
        &fixture.source_path,
        8,
        ExportSpoolLimits::new(1024 * 1024, 8).unwrap(),
        &budget(),
    )
    .unwrap();
    assert_eq!(
        pending_ids(&reopened, &namespace).await,
        vec!["critical-1", "telemetry-1"]
    );
    assert_eq!(
        ExportBacklogState::from_spool(&reopened.status(namespace.clone()).await.unwrap())
            .pending_records,
        2
    );
    let mut wrong = namespace.clone();
    wrong.destination.configuration_revision += 1;
    assert_eq!(
        reopened.status(wrong).await.unwrap_err().code(),
        ExportSpoolErrorCode::NamespaceConflict
    );
    let mut wrong = namespace.clone();
    wrong.provider_kind = "other".into();
    assert_eq!(
        reopened
            .pending(wrong, None, PageLimit::new(1).unwrap())
            .await
            .unwrap_err()
            .code(),
        ExportSpoolErrorCode::NamespaceConflict
    );
    let mut wrong = namespace;
    wrong.source_generation = "other-generation".into();
    assert_eq!(
        reopened.status(wrong).await.unwrap_err().code(),
        ExportSpoolErrorCode::NamespaceConflict
    );
}

#[tokio::test]
async fn expired_source_interval_survives_reopen_and_does_not_consume_operational_budget() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let (source, spool) = fixture.open();
    commit(
        &source,
        vec![
            record("old-1", Durability::Critical),
            record("old-2", Durability::Critical),
        ],
    )
    .await;
    let expired = UtcTimestamp::new(
        instant().into_inner() + Duration::seconds(OPERATIONAL_HISTORY_RETENTION_SECONDS + 1),
    );
    source.maintain_storage_retention(expired).await.unwrap();
    let result =
        ExportIngestor::ingest_once(&source, &spool, destination(), "postgresql", &budget())
            .await
            .unwrap();
    assert_eq!(result.status.pending_records, 0);
    assert_eq!(result.status.gaps.len(), 1);
    assert_eq!(
        (
            result.status.gaps[0].first,
            result.status.gaps[0].last,
            result.status.gaps[0].count(),
            result.status.gaps[0].reason
        ),
        (1, 2, 2, ExportGapReason::SourceExpired)
    );
    assert!(result.status.incomplete);
    assert!(ExportBacklogState::from_spool(&result.status).unconsumed_gaps);
    commit(&source, vec![record("live-3", Durability::Critical)]).await;
    let next = ExportIngestor::ingest_once(&source, &spool, destination(), "postgresql", &budget())
        .await
        .unwrap();
    assert_eq!(next.status.pending_records, 1);
    assert_eq!(next.status.gaps[0].count(), 2);
    assert!(next.status.incomplete);
}

#[tokio::test]
async fn full_spool_defers_critical_without_consuming_authoritative_source_capacity() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let (source, spool) = fixture.open();
    let suffix = "x".repeat(15_000);
    for n in 1..=35 {
        commit(
            &source,
            vec![record(
                &format!("t-{n}-{suffix}"),
                Durability::BestEffortTelemetry,
            )],
        )
        .await;
    }
    let initial =
        ExportIngestor::ingest_once(&source, &spool, destination(), "postgresql", &budget())
            .await
            .unwrap();
    assert!(
        initial
            .status
            .gaps
            .iter()
            .any(|gap| gap.reason == ExportGapReason::TelemetryDropped)
    );
    for n in 1..=35 {
        commit(
            &source,
            vec![record(&format!("c-{n}-{suffix}"), Durability::Critical)],
        )
        .await;
    }
    let result =
        ExportIngestor::ingest_once(&source, &spool, destination(), "postgresql", &budget())
            .await
            .unwrap();
    assert!(result.backpressured);
    let position = result.status.critical.as_ref().unwrap().sequence;
    assert!(position > 0 && position < 35);
    let namespace = uob_application::ExportSpoolNamespace {
        destination: destination(),
        provider_kind: "postgresql".into(),
        source_generation: result.status.source_generation.clone(),
    };
    assert!(
        ExportBacklogState::from_spool(&spool.status(namespace.clone()).await.unwrap())
            .deferred_source
    );
    commit(
        &source,
        vec![record(
            "new-critical-after-export-pressure",
            Durability::Critical,
        )],
    )
    .await;
    let next = source
        .read_committed_records(
            uob_application::CommittedRecordQuery {
                after: result.status.critical.map(|point| point.cursor),
                durability: Durability::Critical,
                limit: PageLimit::new(1).unwrap(),
            },
            &budget(),
        )
        .await
        .unwrap();
    assert_eq!(next.items[0].sequence, position + 1);
    drop(spool);
    let reopened = fixture.open().1;
    assert_eq!(
        reopened
            .status(namespace)
            .await
            .unwrap()
            .critical
            .unwrap()
            .sequence,
        position
    );
}

#[tokio::test]
async fn interior_source_expiry_records_only_missing_positions_not_retained_neighbors() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let (source, spool) = fixture.open();
    let future = UtcTimestamp::new(instant().into_inner() + Duration::days(1));
    let mut first = record("kept-1", Durability::Critical);
    first.committed_at = future;
    let missing = record("expired-2", Durability::Critical);
    let mut third = record("kept-3", Durability::Critical);
    third.committed_at = future;
    commit(&source, vec![first, missing, third]).await;
    source
        .maintain_storage_retention(UtcTimestamp::new(
            instant().into_inner() + Duration::seconds(OPERATIONAL_HISTORY_RETENTION_SECONDS + 1),
        ))
        .await
        .unwrap();
    let ingested =
        ExportIngestor::ingest_once(&source, &spool, destination(), "postgresql", &budget())
            .await
            .unwrap();
    assert_eq!(ingested.status.pending_records, 2);
    assert_eq!(ingested.status.gaps.len(), 1);
    assert_eq!(
        (ingested.status.gaps[0].first, ingested.status.gaps[0].last),
        (2, 2)
    );
    assert_eq!(ingested.status.critical.unwrap().sequence, 3);
    let namespace = uob_application::ExportSpoolNamespace {
        destination: destination(),
        provider_kind: "postgresql".into(),
        source_generation: ingested.status.source_generation,
    };
    assert_eq!(
        pending_ids(&spool, &namespace).await,
        vec!["kept-1", "kept-3"]
    );
}
