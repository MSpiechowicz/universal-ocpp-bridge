use super::*;
use uob_application::{
    AtomicStoreWrite, CommittedRecord, CommittedRecordChunkQuery, CommittedRecordChunkResult,
    CommittedRecordField, CommittedRecordId, CommittedRecordPage, CommittedRecordQuery, Durability,
    EXPORT_RECORD_CHUNK_BYTES, PageLimit, RuntimeResourceBudget, RuntimeResourceLimits,
};

async fn assert_records_unchanged(
    p: &preflight::Policy,
    before: &CommittedRecordPage,
    original_fields: Vec<Vec<Vec<u8>>>,
    budget: &RuntimeResourceBudget,
    query: CommittedRecordQuery,
) {
    let store = Store::open(&p.operational_database, 16).unwrap();
    let after = store.read_committed_records(query, budget).await.unwrap();
    assert_eq!(before.source_generation, after.source_generation);
    assert_eq!(before.resume_cursor, after.resume_cursor);
    assert_eq!(before.high_water, after.high_water);
    assert_eq!(before.items.len(), after.items.len());
    for (descriptor, original) in after.items.iter().zip(original_fields) {
        let mut fields = Vec::new();
        for field in [
            CommittedRecordField::RecordId,
            CommittedRecordField::CommittedAt,
            CommittedRecordField::Payload,
        ] {
            let mut bytes = Vec::new();
            let mut offset = 0;
            while offset < descriptor.token.lengths()[field.index()] {
                let CommittedRecordChunkResult::Data(chunk) = store
                    .read_committed_record_chunk(
                        CommittedRecordChunkQuery {
                            token: descriptor.token.clone(),
                            field,
                            offset,
                            max_bytes: EXPORT_RECORD_CHUNK_BYTES,
                        },
                        budget,
                    )
                    .await
                    .unwrap()
                else {
                    panic!("committed record expired after rollback");
                };
                offset = chunk.next_offset;
                bytes.extend_from_slice(&chunk.bytes);
            }
            fields.push(bytes);
        }
        assert_eq!(fields, original);
    }
    store.shutdown(DEADLINE).await.unwrap();
}

async fn assert_reboot_does_not_retry(f: &Fixture, p: preflight::Policy, manager: Supervisor) {
    drop(manager);
    let mut manager = f.manager().with_preflight_policy(p).unwrap();
    let mut process = Process::default();
    assert_eq!(manager.rollback_automatically(&mut process).await, Code::Ok);
    assert_eq!((process.stops, process.starts), (0, 0));
    assert_eq!(
        manager
            .handle(
                100,
                Request::Stage {
                    digest: f.artifact.digest().into()
                }
            )
            .code,
        Code::RecoveryRequired
    );
    assert_eq!(
        manager
            .observe_failure_and_rollback(
                failures::Policy::default(),
                observation(3, failures::Signal::Watchdog),
                &mut Staging,
                &mut process
            )
            .await
            .unwrap(),
        Code::RecoveryRequired
    );
    assert_eq!(
        manager.handle(100, Request::Status {}).code,
        Code::RecoveryRequired
    );
}

#[test]
fn fallback_preserves_post_promotion_records_export_cursor_and_audits_across_reboot() {
    let _serial = crate::TEST_SERIAL.lock().unwrap();
    run(async {
        let (f, mut manager, p) = setup().await;
        let store = Store::open(&p.operational_database, 16).unwrap();
        let mut write = AtomicStoreWrite::empty();
        for name in [
            "transaction",
            "meter",
            "command",
            "delivery",
            "export-checkpoint",
            "release-audit",
        ] {
            write.committed_records.push(CommittedRecord {
                record_id: CommittedRecordId::new(name).unwrap(),
                durability: Durability::Critical,
                committed_at: serde_json::from_str("\"2026-09-12T12:00:00Z\"").unwrap(),
                record: name.to_owned(),
            });
        }
        store.write_atomic(write).await.unwrap();
        let query = || CommittedRecordQuery {
            after: None,
            limit: PageLimit::new(10).unwrap(),
            durability: Durability::Critical,
        };
        let budget = RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap();
        let before = store
            .read_committed_records(query(), &budget)
            .await
            .unwrap();
        let mut original_fields = Vec::new();
        for descriptor in &before.items {
            let mut fields = Vec::new();
            for field in [
                CommittedRecordField::RecordId,
                CommittedRecordField::CommittedAt,
                CommittedRecordField::Payload,
            ] {
                let mut bytes = Vec::new();
                let mut offset = 0;
                let length = descriptor.token.lengths()[field.index()];
                while offset < length {
                    let CommittedRecordChunkResult::Data(chunk) = store
                        .read_committed_record_chunk(
                            CommittedRecordChunkQuery {
                                token: descriptor.token.clone(),
                                field,
                                offset,
                                max_bytes: EXPORT_RECORD_CHUNK_BYTES,
                            },
                            &budget,
                        )
                        .await
                        .unwrap()
                    else {
                        panic!("committed record expired before rollback");
                    };
                    offset = chunk.next_offset;
                    bytes.extend_from_slice(&chunk.bytes);
                }
                fields.push(bytes);
            }
            original_fields.push(fields);
        }
        store.shutdown(DEADLINE).await.unwrap();
        drop(store);
        let bytes = fs::read(&p.operational_database).unwrap();
        trigger(&mut manager);
        let audit = serde_json::to_vec(&manager.ledger.status().last_operation).unwrap();
        drop(manager); // persisted trigger is sufficient with API and browser unavailable
        let mut manager = f.manager().with_preflight_policy(p.clone()).unwrap();
        let mut process = Process::default();
        assert_eq!(manager.rollback_automatically(&mut process).await, Code::Ok);
        assert_eq!((process.stops, process.starts), (1, 1));
        assert_eq!(
            process.start.unwrap().digest,
            f.previous.compatibility.artifact_digest.as_str()
        );
        assert_eq!(fs::read(&p.operational_database).unwrap(), bytes);
        assert_records_unchanged(&p, &before, original_fields, &budget, query()).await;
        assert_eq!(
            serde_json::to_vec(&manager.ledger.status().last_operation).unwrap(),
            audit
        );
        assert_eq!(
            manager.activation.state().candidate.as_ref().unwrap().phase,
            Phase::Quarantined
        );
        assert_reboot_does_not_retry(&f, p, manager).await;
    });
}
