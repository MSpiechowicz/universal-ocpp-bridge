use super::*;

#[test]
fn query_permissions_and_bounds_fail_closed() {
    let station = station_resource("station-a");
    let source = Arc::new(FakeSource {
        snapshots: vec![snapshot(station.clone()), snapshot(station.clone())],
        oversized_page: true,
        oversized_subscription: true,
        ..FakeSource::default()
    });
    let snapshots_only = ScopedTargetQueryPort::new(
        source.clone(),
        authorization(&station, vec![TargetQueryPermission::StationSnapshots]),
    );

    let error = block_on(snapshots_only.query(TargetQuery::Capabilities(station.clone())))
        .expect_err("ungranted query class must fail");
    assert_eq!(error.code(), TargetPortErrorCode::Unsupported);
    assert_eq!(source.query_calls.load(Ordering::SeqCst), 0);
    assert!(PageLimit::new(101).is_err());

    let error = block_on(snapshots_only.query(TargetQuery::StationSnapshots(snapshot_query(1))))
        .expect_err("source cannot exceed the requested page size");
    assert_eq!(error.code(), TargetPortErrorCode::InvalidRequest);

    let event_port = ScopedTargetQueryPort::new(
        source,
        authorization(&station, vec![TargetQueryPermission::RetainedEvents]),
    );
    let Err(error) = block_on(event_port.subscribe_retained_events(event_query(station, 1))) else {
        panic!("oversized source buffer must fail");
    };
    assert_eq!(error.code(), TargetPortErrorCode::InvalidRequest);
}
