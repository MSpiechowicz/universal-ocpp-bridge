use super::*;

#[test]
fn resumes_across_restarts_and_tracks_the_live_end() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).expect("open SQLite store");
    block_on(store.write_atomic(write_with(
        vec![
            event_for("event-cursor-1", resource(), 1),
            event_for("event-cursor-2", resource(), 2),
        ],
        Vec::new(),
    )))
    .expect("commit retained events");

    let first = block_on(store.read_retained_events(RetainedEventQuery {
        resource: resource(),
        after: None,
        limit: PageLimit::new(1).expect("page limit"),
    }))
    .expect("read first retained page");
    assert_eq!(first.events[0].event_id.as_str(), "event-cursor-1");
    assert!(first.has_more);
    let first_cursor = first.resume_cursor.expect("first durable checkpoint");
    drop(store);

    let reopened = Store::open(database.path(), 8).expect("reopen SQLite store");
    let second = block_on(reopened.read_retained_events(RetainedEventQuery {
        resource: resource(),
        after: Some(first_cursor),
        limit: PageLimit::new(10).expect("page limit"),
    }))
    .expect("resume after restart");
    assert_eq!(second.events[0].event_id.as_str(), "event-cursor-2");
    assert!(!second.has_more);
    let live_cursor = second.resume_cursor.expect("live-end durable checkpoint");

    let at_live_end = block_on(reopened.read_retained_events(RetainedEventQuery {
        resource: resource(),
        after: Some(live_cursor.clone()),
        limit: PageLimit::new(10).expect("page limit"),
    }))
    .expect("read current live end");
    assert!(at_live_end.events.is_empty());
    assert_eq!(at_live_end.resume_cursor, Some(live_cursor.clone()));

    block_on(reopened.write_atomic(write_with(
        vec![event_for("event-cursor-3", resource(), 3)],
        Vec::new(),
    )))
    .expect("append after checkpoint");
    drop(reopened);

    let restarted = Store::open(database.path(), 8).expect("restart SQLite store again");
    let appended = block_on(restarted.read_retained_events(RetainedEventQuery {
        resource: resource(),
        after: Some(live_cursor),
        limit: PageLimit::new(10).expect("page limit"),
    }))
    .expect("resume appended event after restart");
    assert_eq!(appended.events.len(), 1);
    assert_eq!(appended.events[0].event_id.as_str(), "event-cursor-3");
}

#[test]
fn expired_or_wrong_stream_cursor_requires_a_fresh_snapshot() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).expect("open SQLite store");
    block_on(store.write_atomic(write_with(
        vec![
            event_for("event-expired-1", resource(), 1),
            event_for("event-expired-2", resource(), 2),
        ],
        Vec::new(),
    )))
    .expect("commit retained events");
    let first = block_on(store.read_retained_events(RetainedEventQuery {
        resource: resource(),
        after: None,
        limit: PageLimit::new(1).expect("page limit"),
    }))
    .expect("read cursor to expire");
    let expired_cursor = first.resume_cursor.expect("durable checkpoint");

    let other_resource = ResourceRef {
        station_id: text(StationId::new, "station-other"),
        ..resource()
    };
    let wrong_stream = block_on(store.read_retained_events(RetainedEventQuery {
        resource: other_resource,
        after: Some(expired_cursor.clone()),
        limit: PageLimit::new(10).expect("page limit"),
    }))
    .expect_err("cursor must remain bound to its resource stream");
    assert_eq!(wrong_stream.code(), StorageErrorCode::CursorExpired);
    drop(store);

    let connection = Connection::open(database.path()).expect("open database for retention");
    connection
        .execute(
            "DELETE FROM journal_events WHERE event_id = ?1",
            ["event-expired-1"],
        )
        .expect("expire first retained event");
    drop(connection);

    let reopened = Store::open(database.path(), 8).expect("reopen after retention");
    let expired = block_on(reopened.read_retained_events(RetainedEventQuery {
        resource: resource(),
        after: Some(expired_cursor),
        limit: PageLimit::new(10).expect("page limit"),
    }))
    .expect_err("expired cursor must not skip to a newer event");
    assert_eq!(expired.code(), StorageErrorCode::CursorExpired);
    assert_eq!(
        expired.detail(),
        "durable event cursor expired; fetch a fresh snapshot"
    );

    let fresh = block_on(reopened.read_retained_events(RetainedEventQuery {
        resource: resource(),
        after: None,
        limit: PageLimit::new(10).expect("page limit"),
    }))
    .expect("fresh read after caller obtains a snapshot");
    assert_eq!(fresh.events[0].event_id.as_str(), "event-expired-2");
}
