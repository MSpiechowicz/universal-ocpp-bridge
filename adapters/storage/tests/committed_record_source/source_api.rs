use super::*;
use time::Duration;
use uob_application::OPERATIONAL_HISTORY_RETENTION_SECONDS;

fn chunk(
    store: &Store,
    descriptor: &CommittedRecordDescriptor,
    field: CommittedRecordField,
    offset: u64,
    max_bytes: usize,
) -> CommittedRecordChunkResult {
    block_on(store.read_committed_record_chunk(
        CommittedRecordChunkQuery {
            token: descriptor.token.clone(),
            field,
            offset,
            max_bytes,
        },
        &budget(),
    ))
    .unwrap()
}

fn data(result: CommittedRecordChunkResult) -> uob_application::BudgetedRecordChunk {
    match result {
        CommittedRecordChunkResult::Data(value) => value,
        CommittedRecordChunkResult::Expired => panic!("unexpected source expiry"),
    }
}

#[test]
fn multi_megabyte_payload_and_long_id_are_read_byte_exact_in_bounded_parts() {
    let db = Database::new();
    let store = Store::open(&db.0, 8).unwrap();
    let identifier = format!("id-{}é", "x".repeat(132_000));
    let raw = format!("{}é\\\"end", "a".repeat(2_100_000));
    commit(
        &store,
        vec![record(&identifier, Durability::Critical, raw.clone())],
    )
    .unwrap();

    let page =
        block_on(store.read_committed_records(query(Durability::Critical, None, 1), &budget()))
            .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.source_generation, page.items[0].token.generation());
    let descriptor = &page.items[0];
    assert_eq!(descriptor.record_id_len, identifier.len() as u64);
    assert_eq!(
        descriptor.payload_len,
        serde_json::to_vec(&raw).unwrap().len() as u64
    );

    for (field, expected) in [
        (CommittedRecordField::RecordId, identifier.into_bytes()),
        (
            CommittedRecordField::Payload,
            serde_json::to_vec(&raw).unwrap(),
        ),
    ] {
        let mut actual = Vec::new();
        let mut offset = 0;
        loop {
            let part = data(chunk(
                &store,
                descriptor,
                field,
                offset,
                EXPORT_RECORD_CHUNK_BYTES,
            ));
            assert_eq!(part.offset, offset);
            assert!(part.bytes.len() <= EXPORT_RECORD_CHUNK_BYTES);
            actual.extend_from_slice(&part.bytes);
            offset = part.next_offset;
            if part.end_of_field {
                break;
            }
        }
        assert_eq!(actual, expected);
    }
    let committed = data(chunk(
        &store,
        descriptor,
        CommittedRecordField::CommittedAt,
        0,
        64,
    ));
    assert_eq!(committed.bytes.len() as u64, descriptor.committed_at_len);
}

#[test]
fn utf8_split_offset_errors_and_foreign_token_never_claim_expiry() {
    let db = Database::new();
    let store = Store::open(&db.0, 8).unwrap();
    let identifier = format!("{}é", "x".repeat(EXPORT_RECORD_CHUNK_BYTES - 1));
    commit(
        &store,
        vec![record(&identifier, Durability::Critical, "part".into())],
    )
    .unwrap();
    let page =
        block_on(store.read_committed_records(query(Durability::Critical, None, 1), &budget()))
            .unwrap();
    let descriptor = &page.items[0];
    let first = data(chunk(
        &store,
        descriptor,
        CommittedRecordField::RecordId,
        0,
        EXPORT_RECORD_CHUNK_BYTES,
    ));
    assert_eq!(first.bytes.last(), Some(&0xc3));
    let last = data(chunk(
        &store,
        descriptor,
        CommittedRecordField::RecordId,
        first.next_offset,
        8,
    ));
    assert_eq!(last.bytes, [0xa9]);
    assert!(last.end_of_field);
    let empty = data(chunk(
        &store,
        descriptor,
        CommittedRecordField::RecordId,
        last.next_offset,
        8,
    ));
    assert!(empty.bytes.is_empty() && empty.end_of_field);

    for offset in [descriptor.record_id_len + 1, u64::MAX] {
        let err = block_on(store.read_committed_record_chunk(
            CommittedRecordChunkQuery {
                token: descriptor.token.clone(),
                field: CommittedRecordField::RecordId,
                offset,
                max_bytes: 8,
            },
            &budget(),
        ))
        .unwrap_err();
        assert_eq!(err.code(), StorageErrorCode::InvalidRequest);
    }
    let err = block_on(store.read_committed_record_chunk(
        CommittedRecordChunkQuery {
            token: descriptor.token.clone(),
            field: CommittedRecordField::RecordId,
            offset: 0,
            max_bytes: EXPORT_RECORD_CHUNK_BYTES + 1,
        },
        &budget(),
    ))
    .unwrap_err();
    assert_eq!(err.code(), StorageErrorCode::InvalidRequest);

    let fake = uob_application::CommittedRecordReadToken::new(
        page.source_generation.clone().into(),
        Durability::Critical,
        1_000,
        999,
        descriptor.token.lengths(),
        descriptor.token.authenticator(),
    );
    let err = block_on(store.read_committed_record_chunk(
        CommittedRecordChunkQuery {
            token: fake,
            field: CommittedRecordField::Payload,
            offset: 0,
            max_bytes: 8,
        },
        &budget(),
    ))
    .unwrap_err();
    assert_eq!(err.code(), StorageErrorCode::InvalidRequest);

    let other = Database::new();
    let foreign = Store::open(&other.0, 8).unwrap();
    let err = block_on(foreign.read_committed_record_chunk(
        CommittedRecordChunkQuery {
            token: descriptor.token.clone(),
            field: CommittedRecordField::RecordId,
            offset: 0,
            max_bytes: 8,
        },
        &budget(),
    ))
    .unwrap_err();
    assert_eq!(err.code(), StorageErrorCode::InvalidRequest);
}

#[test]
fn retention_between_chunks_and_operational_write_interleave() {
    let db = Database::new();
    let store = Store::open(&db.0, 8).unwrap();
    commit(
        &store,
        vec![record("old", Durability::Critical, "x".repeat(1_500_000))],
    )
    .unwrap();
    let page =
        block_on(store.read_committed_records(query(Durability::Critical, None, 1), &budget()))
            .unwrap();
    let descriptor = &page.items[0];
    let first = data(chunk(
        &store,
        descriptor,
        CommittedRecordField::Payload,
        0,
        EXPORT_RECORD_CHUNK_BYTES,
    ));
    assert_eq!(first.bytes.len(), EXPORT_RECORD_CHUNK_BYTES);
    let mut recent = record("new", Durability::Critical, "alive".into());
    recent.committed_at = UtcTimestamp::new(instant().into_inner() + Duration::days(1));
    commit(&store, vec![recent]).unwrap();
    let second = data(chunk(
        &store,
        descriptor,
        CommittedRecordField::Payload,
        first.next_offset,
        64,
    ));
    assert_eq!(second.bytes.len(), 64);
    let future = UtcTimestamp::new(
        instant().into_inner() + Duration::seconds(OPERATIONAL_HISTORY_RETENTION_SECONDS + 1),
    );
    block_on(store.maintain_storage_retention(future)).unwrap();
    let result = chunk(
        &store,
        descriptor,
        CommittedRecordField::Payload,
        second.next_offset,
        64,
    );
    assert!(matches!(result, CommittedRecordChunkResult::Expired));
    let continued =
        block_on(store.read_committed_records(query(Durability::Critical, None, 2), &budget()))
            .unwrap();
    assert_eq!(continued.items[0].sequence, 2);
    assert_eq!(continued.lost_records, 1);
    assert_eq!(continued.high_water, 2);
}
