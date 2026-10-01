use super::*;
use std::os::unix::fs::MetadataExt;
use uob_application::{
    CommittedRecord, CommittedRecordId, Durability, ExportIngestor, ExportSpool,
    ExportSpoolNamespace, PageLimit, RetainedEventQuery, RuntimeResourceBudget,
    RuntimeResourceLimits, StorageAdmissionState,
};
use uob_contracts::{
    ArtifactDigest, DataPointValue, Environment, ExportDestination, ExportDestinationId,
    ExportPayload, ExportRecord, ExportRecordId, ExportRecordIdentity, ExportRecordMetadata,
    Freshness, PointId, ProcessInstanceId, Quality, QualityLevel, ReleaseId, RuntimeIdentity,
    TransactionState, TypedValue,
};
use uob_storage_adapter::{ExportSpoolLimits, SqliteExportSpool};

// /dev/shm is a different device in the normal Linux test environment, but is tmpfs,
// not a substitute for a production durable, independently quota-limited volume.
struct SeparateSpool {
    directory: PathBuf,
}

impl SeparateSpool {
    fn new(operational_database: &std::path::Path) -> Option<Self> {
        let volume = std::path::Path::new("/dev/shm");
        if fs::metadata(volume).ok()?.dev()
            == fs::metadata(operational_database.parent()?).ok()?.dev()
        {
            return None;
        }
        let directory = volume.join(format!("uob-charging-spool-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        Some(Self { directory })
    }

    fn open(&self, operational_database: &std::path::Path) -> SqliteExportSpool {
        SqliteExportSpool::open_with_limits(
            &self.directory,
            operational_database,
            8,
            ExportSpoolLimits::new(8 * 1024 * 1024, 8).unwrap(),
            &budget(),
        )
        .unwrap()
    }
}

impl Drop for SeparateSpool {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn budget() -> RuntimeResourceBudget {
    RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap()
}

fn namespace(generation: &str) -> ExportSpoolNamespace {
    ExportSpoolNamespace {
        destination: ExportDestination {
            destination_id: ExportDestinationId::new("acceptance-export").unwrap(),
            configuration_revision: 1,
        },
        provider_kind: "postgresql".into(),
        source_generation: generation.into(),
    }
}

fn pressure_record(id: &str, bytes: usize) -> CommittedRecord<ExportRecord> {
    let at = UtcTimestamp::new(time::OffsetDateTime::now_utc());
    CommittedRecord {
        record_id: CommittedRecordId::new(id).unwrap(),
        durability: Durability::Critical,
        committed_at: at,
        record: ExportRecord::new(
            ExportRecordMetadata {
                identity: ExportRecordIdentity::root(ExportRecordId::new(id).unwrap()),
                schema_version: ExportRecord::SCHEMA_VERSION,
                runtime: RuntimeIdentity {
                    environment: Environment::Demo,
                    release_id: ReleaseId::new("acceptance-test").unwrap(),
                    release_digest: ArtifactDigest::new("sha256:test").unwrap(),
                    process_instance_id: ProcessInstanceId::new("acceptance-process").unwrap(),
                },
                resource: ResourceRef {
                    bridge_id: BridgeId::new("bridge-1").unwrap(),
                    station_id: StationId::new("station-a").unwrap(),
                    resource: None,
                    native_protocol_reference: None,
                },
                source_time: None,
                observed_at: at,
                sequence: 1,
                correlation_id: None,
            },
            ExportPayload::Measurement(DataPointValue {
                point_id: PointId::new("pressure").unwrap(),
                value: Some(TypedValue::Text("x".repeat(bytes))),
                source_time: None,
                observed_at: at,
                quality: Quality {
                    level: QualityLevel::Good,
                    reason: None,
                },
                freshness: Freshness::Unknown,
                measurement: None,
            }),
        ),
    }
}

async fn charging_session(port: u16, label: &str) -> i32 {
    let (mut socket, _) = connect_async(request(port)).await.unwrap();
    assert_eq!(
        call(
            &mut socket,
            serde_json::json!([2, format!("{label}-boot"), "BootNotification", {
                "chargePointVendor": "LiveVendor", "chargePointModel": "LiveModel"
            }]),
        )
        .await[2]["status"],
        "Accepted"
    );
    let instant = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let started = call(
        &mut socket,
        serde_json::json!([2, format!("{label}-start"), "StartTransaction", {
            "connectorId": 1, "idTag": "TEST-TAG", "meterStart": 0, "timestamp": instant
        }]),
    )
    .await;
    assert_eq!(started[0], 3);
    let transaction_id = i32::try_from(started[2]["transactionId"].as_i64().unwrap())
        .expect("StartTransaction transactionId must fit in i32");
    let stopped = call(
        &mut socket,
        serde_json::json!([2, format!("{label}-stop"), "StopTransaction", {
            "transactionId": transaction_id, "meterStop": 10, "timestamp": instant
        }]),
    )
    .await;
    assert_eq!(stopped[0], 3);
    assert_eq!(stopped[1], format!("{label}-stop"));
    drop(socket);
    transaction_id
}

async fn operational_evidence(path: &std::path::Path, transaction_ids: &[i32]) {
    let store: SqliteOperationalStore<
        serde_json::Value,
        StationEvent,
        TransactionSnapshot,
        String,
    > = SqliteOperationalStore::open(path, 16).unwrap();
    let resource = ResourceRef {
        bridge_id: BridgeId::new("bridge-1").unwrap(),
        station_id: StationId::new("station-a").unwrap(),
        resource: None,
        native_protocol_reference: None,
    };
    let station = store.station_snapshot(resource).await.unwrap().unwrap();
    let connector = station.resources[0].resource.clone();
    for transaction_id in transaction_ids {
        assert!(station.transactions.iter().any(|transaction| {
            transaction.state == TransactionState::Ended
                && transaction
                    .ocpp16
                    .as_ref()
                    .is_some_and(|evidence| evidence.transaction_id == *transaction_id)
        }));
    }
    let journal = store
        .read_retained_events(RetainedEventQuery {
            resource: connector,
            after: None,
            limit: PageLimit::new(100).unwrap(),
        })
        .await
        .unwrap();
    for transaction_id in transaction_ids {
        assert!(journal.events.iter().any(|event| {
            matches!(&event.payload, StationEvent::Transaction(transaction)
                if transaction.state == TransactionState::Ended
                    && transaction.ocpp16.as_ref().is_some_and(|evidence|
                        evidence.transaction_id == *transaction_id))
        }));
    }
    let retention = store.storage_retention_status().await.unwrap();
    assert_eq!(
        retention.new_session_admission,
        StorageAdmissionState::Available,
        "optional spool pressure must not exhaust operational admission"
    );
    store.shutdown(Duration::from_secs(2)).await.unwrap();
}

async fn pending_replay_count(spool: &SqliteExportSpool, namespace: &ExportSpoolNamespace) -> u64 {
    let mut after = None;
    let mut replayed = 0_u64;
    loop {
        let page = spool
            .pending(namespace.clone(), after, PageLimit::new(100).unwrap())
            .await
            .unwrap();
        for record in &page.items {
            assert_eq!(record.position.durability, Durability::Critical);
            if let Some(previous) = after {
                assert!(
                    record.position.sequence > previous.sequence,
                    "pending replay must advance without duplicates"
                );
            }
            after = Some(record.position);
            replayed += 1;
        }
        if !page.has_more {
            break;
        }
        after = page.resume;
    }
    replayed
}

#[tokio::test]
async fn isolated_optional_spool_backpressure_does_not_block_real_charging_or_restart() {
    let fixture = Fixture::new();
    let database = fixture.state().join("charging.sqlite3");
    let Some(volume) = SeparateSpool::new(&database) else {
        eprintln!("skipping isolated spool test: /dev/shm is not a separate device");
        return;
    };
    let spool = volume.open(&database);
    let mut child = fixture.start();
    ready(&mut child, fixture.charging).await;

    // The host-owned export path is not enabled in uob serve. Run its actual bounded
    // source reads while the service processes a charging session on the same database.
    let source: SqliteOperationalStore<
        serde_json::Value,
        StationEvent,
        TransactionSnapshot,
        ExportRecord,
    > = SqliteOperationalStore::open(&database, 16).unwrap();
    let mut write = AtomicStoreWrite::empty();
    write
        .committed_records
        .push(pressure_record("large", 2 * 1024 * 1024));
    source.write_atomic(write).await.unwrap();
    let export_budget = budget();
    let (copied, first) = tokio::join!(
        ExportIngestor::ingest_once(
            &source,
            &spool,
            namespace("").destination,
            "postgresql",
            &export_budget,
        ),
        charging_session(fixture.charging, "streaming")
    );
    let copied = copied.unwrap();
    assert!(copied.status.pending_records >= 1);
    assert!(!copied.backpressured);

    let mut write = AtomicStoreWrite::empty();
    write
        .committed_records
        .push(pressure_record("impossible", 9 * 1024 * 1024));
    source.write_atomic(write).await.unwrap();
    let (deferred, during_pressure) = tokio::join!(
        ExportIngestor::ingest_once(
            &source,
            &spool,
            namespace("").destination,
            "postgresql",
            &export_budget,
        ),
        charging_session(fixture.charging, "deferred")
    );
    let deferred = deferred.unwrap();
    assert!(deferred.backpressured);
    let namespace = namespace(&deferred.status.source_generation);
    assert!(
        deferred.status.critical_high_water > deferred.status.critical.as_ref().unwrap().sequence
    );
    let pressure = spool.status(namespace.clone()).await.unwrap();
    assert_eq!(pressure, deferred.status);
    stop(child);
    operational_evidence(&database, &[first, during_pressure]).await;

    drop(spool);
    let reopened = volume.open(&database);
    assert_eq!(reopened.status(namespace.clone()).await.unwrap(), pressure);
    assert!(
        ExportIngestor::ingest_once(
            &source,
            &reopened,
            namespace.destination.clone(),
            "postgresql",
            &export_budget,
        )
        .await
        .unwrap()
        .backpressured
    );

    let mut child = fixture.start();
    ready(&mut child, fixture.charging).await;
    let after_restart = charging_session(fixture.charging, "restarted").await;
    assert_ne!(
        after_restart, first,
        "operational transaction IDs survive restart"
    );
    stop(child);
    operational_evidence(&database, &[first, during_pressure, after_restart]).await;

    assert_eq!(
        pending_replay_count(&reopened, &namespace).await,
        pressure.pending_records
    );
    assert_eq!(reopened.status(namespace).await.unwrap(), pressure);
    source.shutdown(Duration::from_secs(2)).await.unwrap();
}
