use super::*;

#[test]
fn atomic_failure_exposes_no_command_event_delivery_or_committed_record() {
    let store = MemoryStore::default();
    store.fail_next_write();

    let error = block_on(store.write_atomic(populated_write())).expect_err("injected failure");
    assert_eq!(error.code(), StorageErrorCode::Unavailable);
    assert!(
        block_on(store.command_by_request_id(text(RequestId::new, "request-1")))
            .expect("command lookup")
            .is_none()
    );
    assert!(
        block_on(store.read_retained_events(RetainedEventQuery {
            resource: resource(),
            after: None,
            limit: PageLimit::new(10).expect("bounded page"),
        }))
        .expect("event read")
        .items
        .is_empty()
    );
    assert!(
        block_on(store.recover(RecoveryQuery {
            limit: PageLimit::new(10).expect("bounded page"),
        }))
        .expect("recovery read")
        .pending_deliveries
        .is_empty()
    );
    assert!(
        block_on(store.recover(RecoveryQuery {
            limit: PageLimit::new(10).expect("bounded page"),
        }))
        .expect("recovery read")
        .authorization
        .is_empty()
    );
    assert!(
        block_on(store.read_committed_records(CommittedRecordQuery {
            after: None,
            limit: PageLimit::new(10).expect("bounded page"),
            include_best_effort_telemetry: true,
        }))
        .expect("committed record read")
        .items
        .is_empty()
    );
}

#[test]
fn atomic_success_and_identical_retry_are_visible_without_duplication() {
    let store = MemoryStore::default();

    let first = block_on(store.write_atomic(populated_write())).expect("first commit");
    assert_eq!(first.command, Some(CommandAdmissionOutcome::Admitted));
    let retry = block_on(store.write_atomic(populated_write())).expect("idempotent retry");
    assert_eq!(retry.command, Some(CommandAdmissionOutcome::Duplicate));
    let mut conflicting_write = populated_write();
    conflicting_write
        .command
        .as_mut()
        .expect("test command")
        .expires_at = timestamp(8);
    let conflict = block_on(store.write_atomic(conflicting_write)).expect_err("ID conflict");
    assert_eq!(conflict.code(), StorageErrorCode::Conflict);

    let recovery = block_on(store.recover(RecoveryQuery {
        limit: PageLimit::new(10).expect("bounded page"),
    }))
    .expect("recovery read");
    assert_eq!(recovery.active_commands.len(), 1);
    assert_eq!(recovery.pending_deliveries.len(), 1);
    assert_eq!(recovery.authorization.len(), 1);

    let critical_page = block_on(store.read_committed_records(CommittedRecordQuery {
        after: None,
        limit: PageLimit::new(1).expect("bounded page"),
        include_best_effort_telemetry: false,
    }))
    .expect("bounded committed record read");
    assert_eq!(critical_page.items.len(), 1);
    assert_eq!(critical_page.items[0].durability, Durability::Critical);
}

#[test]
fn bounded_reads_cursor_expiry_and_sanitized_error_mapping_are_explicit() {
    assert!(PageLimit::new(0).is_err());
    assert!(PageLimit::new(101).is_err());
    let store = MemoryStore::default();

    let expired = block_on(store.read_retained_events(RetainedEventQuery {
        resource: resource(),
        after: Some(text(RetainedEventCursor::new, "expired")),
        limit: PageLimit::new(1).expect("bounded page"),
    }))
    .expect_err("expired cursor");
    assert_eq!(expired.code(), StorageErrorCode::CursorExpired);
    assert_eq!(expired.detail(), "event cursor expired after retention");
    assert!(std::error::Error::source(&expired).is_none());
}

#[test]
fn application_consumer_accepts_two_storage_adapters_without_changes() {
    assert_store_is_replaceable(&MemoryStore::default());
    assert_store_is_replaceable(&ReplacementMemoryStore::default());
}
