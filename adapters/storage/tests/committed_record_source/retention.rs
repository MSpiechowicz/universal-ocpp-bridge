use super::*;
use time::Duration;
use uob_application::OPERATIONAL_HISTORY_RETENTION_SECONDS;

#[test]
fn expiry_preserves_empty_tail_loss_and_resumes_deleted_checkpoints() {
    let db = Database::new();
    let store = Store::open(&db.0, 8).unwrap();
    let empty =
        block_on(store.read_committed_records(query(Durability::Critical, None, 1), &budget()))
            .unwrap();
    commit(
        &store,
        (1..=3)
            .map(|n| record(&format!("old-{n}"), Durability::Critical, "old".into()))
            .collect(),
    )
    .unwrap();
    let consumed = block_on(store.read_committed_records(
        query(Durability::Critical, Some(empty.resume_cursor.clone()), 1),
        &budget(),
    ))
    .unwrap();
    let end =
        block_on(store.read_committed_records(query(Durability::Critical, None, 3), &budget()))
            .unwrap();
    assert_eq!(consumed.items[0].sequence, 1);
    assert_eq!(end.items.last().unwrap().sequence, 3);
    let expires = UtcTimestamp::new(
        instant().into_inner() + Duration::seconds(OPERATIONAL_HISTORY_RETENTION_SECONDS + 1),
    );
    block_on(store.maintain_storage_retention(expires)).unwrap();
    drop(store);

    let reopened = Store::open(&db.0, 8).unwrap();
    let fresh =
        block_on(reopened.read_committed_records(query(Durability::Critical, None, 1), &budget()))
            .unwrap();
    assert!(fresh.items.is_empty());
    assert_eq!(
        (fresh.high_water, fresh.expired_prefix, fresh.lost_records),
        (3, 3, 3)
    );
    let resumed = block_on(reopened.read_committed_records(
        query(Durability::Critical, Some(consumed.resume_cursor), 1),
        &budget(),
    ))
    .unwrap();
    assert!(resumed.items.is_empty());
    assert_eq!(resumed.lost_records, 2);
    assert_eq!(resumed.resume_cursor, fresh.resume_cursor);
    let at_end = block_on(reopened.read_committed_records(
        query(Durability::Critical, Some(end.resume_cursor), 1),
        &budget(),
    ))
    .unwrap();
    assert!(at_end.items.is_empty());
    assert_eq!(at_end.lost_records, 0);
    assert_eq!(
        block_on(reopened.read_committed_records(
            query(
                Durability::Critical,
                Some(CommittedRecordCursor::new("uob:record:v1:wrong:0:0").unwrap()),
                1
            ),
            &budget()
        ))
        .unwrap_err()
        .code(),
        StorageErrorCode::CursorExpired
    );
}

#[test]
fn deleted_last_checkpoint_resumes_at_later_records_and_reports_only_subsequent_loss() {
    let db = Database::new();
    let store = Store::open(&db.0, 8).unwrap();
    commit(
        &store,
        vec![record("old-1", Durability::Critical, "first".into())],
    )
    .unwrap();
    let checkpoint =
        block_on(store.read_committed_records(query(Durability::Critical, None, 1), &budget()))
            .unwrap()
            .resume_cursor;
    let mut recent = record("live-3", Durability::Critical, "last".into());
    recent.committed_at = UtcTimestamp::new(instant().into_inner() + Duration::days(1));
    commit(
        &store,
        vec![
            record("old-2", Durability::Critical, "missing".into()),
            recent,
        ],
    )
    .unwrap();
    let expires = UtcTimestamp::new(
        instant().into_inner() + Duration::seconds(OPERATIONAL_HISTORY_RETENTION_SECONDS + 1),
    );
    block_on(store.maintain_storage_retention(expires)).unwrap();
    drop(store);

    let reopened = Store::open(&db.0, 8).unwrap();
    let resumed = block_on(
        reopened
            .read_committed_records(query(Durability::Critical, Some(checkpoint), 1), &budget()),
    )
    .unwrap();
    assert_eq!(
        resumed
            .items
            .iter()
            .map(|item| item.sequence)
            .collect::<Vec<_>>(),
        vec![3]
    );
    assert_eq!(payload(&reopened, &resumed.items[0]), "last");
    assert_eq!(resumed.lost_records, 1);
    assert_eq!(resumed.expired_prefix, 2);
    let at_end = block_on(reopened.read_committed_records(
        query(Durability::Critical, Some(resumed.resume_cursor), 1),
        &budget(),
    ))
    .unwrap();
    assert!(at_end.items.is_empty());
    assert_eq!(at_end.lost_records, 0);
}

#[test]
fn deleted_interior_checkpoint_reports_loss_after_it_without_recounting_it() {
    let db = Database::new();
    let store = Store::open(&db.0, 8).unwrap();
    let mut recent_first = record("live-1", Durability::Critical, "first".into());
    recent_first.committed_at = UtcTimestamp::new(instant().into_inner() + Duration::days(1));
    let mut recent_last = record("live-4", Durability::Critical, "last".into());
    recent_last.committed_at = recent_first.committed_at;
    commit(
        &store,
        vec![
            recent_first,
            record("old-2", Durability::Critical, "checkpoint".into()),
            record("old-3", Durability::Critical, "missing".into()),
            recent_last,
        ],
    )
    .unwrap();
    let first =
        block_on(store.read_committed_records(query(Durability::Critical, None, 2), &budget()))
            .unwrap();
    assert_eq!(
        first
            .items
            .iter()
            .map(|item| item.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let expires = UtcTimestamp::new(
        instant().into_inner() + Duration::seconds(OPERATIONAL_HISTORY_RETENTION_SECONDS + 1),
    );
    block_on(store.maintain_storage_retention(expires)).unwrap();
    drop(store);

    let reopened = Store::open(&db.0, 8).unwrap();
    let resumed = block_on(reopened.read_committed_records(
        query(Durability::Critical, Some(first.resume_cursor), 1),
        &budget(),
    ))
    .unwrap();
    assert_eq!(
        resumed
            .items
            .iter()
            .map(|item| item.sequence)
            .collect::<Vec<_>>(),
        vec![4]
    );
    assert_eq!(payload(&reopened, &resumed.items[0]), "last");
    assert_eq!(resumed.expired_prefix, 0);
    assert_eq!(resumed.lost_records, 1);
}

#[test]
fn interior_expiry_reports_the_missing_position_without_erasing_later_records() {
    let db = Database::new();
    let store = Store::open(&db.0, 8).unwrap();
    let mut recent_first = record("live-1", Durability::Critical, "first".into());
    recent_first.committed_at = UtcTimestamp::new(instant().into_inner() + Duration::days(1));
    let mut recent_last = record("live-3", Durability::Critical, "last".into());
    recent_last.committed_at = recent_first.committed_at;
    commit(
        &store,
        vec![
            recent_first,
            record("expired-2", Durability::Critical, "expired".into()),
            recent_last,
        ],
    )
    .unwrap();
    let expires = UtcTimestamp::new(
        instant().into_inner() + Duration::seconds(OPERATIONAL_HISTORY_RETENTION_SECONDS + 1),
    );
    block_on(store.maintain_storage_retention(expires)).unwrap();
    let first =
        block_on(store.read_committed_records(query(Durability::Critical, None, 1), &budget()))
            .unwrap();
    assert_eq!(
        (
            first.items[0].sequence,
            first.expired_prefix,
            first.lost_records
        ),
        (1, 0, 0)
    );
    let last = block_on(store.read_committed_records(
        query(Durability::Critical, Some(first.resume_cursor), 1),
        &budget(),
    ))
    .unwrap();
    assert_eq!(last.items[0].sequence, 3);
    assert_eq!(payload(&store, &last.items[0]), "last");
    assert_eq!(last.lost_records, 1);
}
