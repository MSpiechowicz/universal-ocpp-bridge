use super::*;

#[test]
fn duplicate_record_rolls_back_sequence_and_invalid_cursor_is_explicit() {
    let db = Database::new();
    let store = Store::open(&db.0, 8).unwrap();
    commit(
        &store,
        vec![record("same", Durability::Critical, "one".into())],
    )
    .unwrap();
    assert!(
        commit(
            &store,
            vec![
                record("new", Durability::Critical, "two".into()),
                record("same", Durability::Critical, "duplicate".into())
            ]
        )
        .is_err()
    );
    commit(
        &store,
        vec![record("last", Durability::Critical, "three".into())],
    )
    .unwrap();
    let page =
        block_on(store.read_committed_records(query(Durability::Critical, None, 3), &budget()))
            .unwrap();
    assert_eq!(
        page.items.iter().map(|v| v.sequence).collect::<Vec<_>>(),
        vec![1, 2]
    );
    let invalid =
        CommittedRecordCursor::new(format!("{}999", page.resume_cursor.as_str())).unwrap();
    assert_eq!(
        block_on(
            store.read_committed_records(query(Durability::Critical, Some(invalid), 1), &budget())
        )
        .unwrap_err()
        .code(),
        StorageErrorCode::CursorExpired
    );
    let malformed =
        CommittedRecordCursor::new(format!("{}x", page.resume_cursor.as_str())).unwrap();
    assert_eq!(
        block_on(
            store
                .read_committed_records(query(Durability::Critical, Some(malformed), 1), &budget())
        )
        .unwrap_err()
        .code(),
        StorageErrorCode::CursorExpired
    );
    let negative = CommittedRecordCursor::new(format!(
        "{}-1",
        page.resume_cursor.as_str().strip_suffix('2').unwrap()
    ))
    .unwrap();
    assert_eq!(
        block_on(
            store.read_committed_records(query(Durability::Critical, Some(negative), 1), &budget())
        )
        .unwrap_err()
        .code(),
        StorageErrorCode::CursorExpired
    );
}
