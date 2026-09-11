use super::*;
use uob_contracts::{BridgeId, StationId, TargetInstanceId};

fn filter(station: &str) -> CaptureFilter {
    CaptureFilter {
        bridge: BridgeId::new("bridge").unwrap(),
        station: Some(StationId::new(station).unwrap()),
        target: Some(TargetInstanceId::new("target-a").unwrap()),
    }
}
fn grant(permission: CapturePermission, station: &str) -> CaptureGrant {
    CaptureGrant::new(
        filter(station).bridge,
        vec![permission],
        Some(vec![StationId::new(station).unwrap()]),
        Some(vec![TargetInstanceId::new("target-a").unwrap()]),
    )
    .unwrap()
}
#[test]
fn disabled_and_unauthorized_requests_do_not_allocate_or_start_worker() {
    let manager = CaptureManager::new(false);
    assert_eq!(
        manager
            .start(
                &grant(CapturePermission::Capture, "a"),
                filter("a"),
                CaptureLevel::Metadata,
                None
            )
            .unwrap_err(),
        CaptureError::Disabled
    );
    assert!(!manager.state.lock().unwrap().worker_running);
    let manager = CaptureManager::new(true);
    assert_eq!(
        manager
            .start(
                &grant(CapturePermission::Read, "a"),
                filter("a"),
                CaptureLevel::Metadata,
                None
            )
            .unwrap_err(),
        CaptureError::Forbidden
    );
    assert_eq!(
        manager
            .start(
                &grant(CapturePermission::Capture, "a"),
                filter("b"),
                CaptureLevel::Metadata,
                None
            )
            .unwrap_err(),
        CaptureError::Forbidden
    );
    assert!(!manager.state.lock().unwrap().worker_running);
    assert!(manager.state.lock().unwrap().session.is_none());
}
#[test]
fn default_maximum_payload_and_target_constraints() {
    let manager = CaptureManager::new(true);
    let all = CaptureGrant::new(
        filter("a").bridge.clone(),
        vec![CapturePermission::Capture],
        None,
        None,
    )
    .unwrap();
    let mut broad = filter("a");
    broad.station = None;
    assert_eq!(
        manager
            .start(&all, broad, CaptureLevel::RedactedPayload, None)
            .unwrap_err(),
        CaptureError::Invalid
    );
    assert_eq!(
        manager
            .start(
                &all,
                filter("a"),
                CaptureLevel::Metadata,
                Some(Duration::ZERO)
            )
            .unwrap_err(),
        CaptureError::Invalid
    );
    assert_eq!(
        manager
            .start(
                &all,
                filter("a"),
                CaptureLevel::Metadata,
                Some(Duration::from_secs(1801))
            )
            .unwrap_err(),
        CaptureError::Invalid
    );
    let mut wrong_target = filter("a");
    wrong_target.target = Some(TargetInstanceId::new("target-b").unwrap());
    assert_eq!(
        manager
            .start(
                &grant(CapturePermission::Capture, "a"),
                wrong_target,
                CaptureLevel::Metadata,
                None
            )
            .unwrap_err(),
        CaptureError::Forbidden
    );
    let status = manager
        .start(&all, filter("a"), CaptureLevel::Metadata, None)
        .unwrap();
    assert_eq!(status.remaining, DEFAULT_CAPTURE_DURATION);
    assert!(manager.accepts(&filter("a"), CaptureLevel::Metadata));
    assert!(!manager.accepts(&filter("a"), CaptureLevel::RedactedPayload));
    assert!(!manager.accepts(&filter("b"), CaptureLevel::Metadata));
}
#[test]
fn read_control_stream_and_export_scopes_remain_separate() {
    let manager = CaptureManager::new(true);
    let control = grant(CapturePermission::Capture, "a");
    let read = grant(CapturePermission::Read, "a");
    let status = manager
        .start(&control, filter("a"), CaptureLevel::RedactedPayload, None)
        .unwrap();
    assert_eq!(
        manager.status(&control).unwrap_err(),
        CaptureError::Forbidden
    );
    assert_eq!(manager.stop(&read, status.id), Err(CaptureError::Forbidden));
    assert_eq!(
        manager.extend(&read, status.id, Duration::from_secs(10)),
        Err(CaptureError::Forbidden)
    );
    assert_eq!(
        manager
            .status(&grant(CapturePermission::Read, "b"))
            .unwrap_err(),
        CaptureError::Forbidden
    );
    for export in [false, true] {
        assert_eq!(
            manager
                .lease(&grant(CapturePermission::Read, "b"), status.id, export)
                .unwrap_err(),
            CaptureError::Forbidden
        );
        let lease = manager.lease(&read, status.id, export).unwrap();
        assert!(lease.permits(&filter("a")));
        assert!(!lease.permits(&filter("b")));
    }
    let before = manager
        .state
        .lock()
        .unwrap()
        .session
        .as_ref()
        .unwrap()
        .deadline;
    manager.status(&read).unwrap();
    assert_eq!(
        manager
            .state
            .lock()
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .deadline,
        before
    );
    manager
        .extend(&control, status.id, MAX_CAPTURE_DURATION)
        .unwrap();
    assert!(manager.status(&read).unwrap().remaining > DEFAULT_CAPTURE_DURATION);
    assert_eq!(
        manager
            .start(&control, filter("a"), CaptureLevel::Metadata, None)
            .unwrap_err(),
        CaptureError::Conflict
    );
    assert_eq!(
        manager.status(&read).unwrap().level,
        CaptureLevel::RedactedPayload
    );
}
#[test]
fn concurrent_starts_share_one_slot() {
    let manager = CaptureManager::new(true);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..12)
            .map(|_| {
                let manager = &manager;
                scope.spawn(move || {
                    manager.start(
                        &grant(CapturePermission::Capture, "a"),
                        filter("a"),
                        CaptureLevel::Metadata,
                        None,
                    )
                })
            })
            .collect();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Err(CaptureError::Conflict)))
                .count(),
            11
        );
    });
}
#[test]
fn stopped_subscribers_cannot_read_but_bounded_exports_delay_release() {
    let manager = CaptureManager::new(true);
    let control = grant(CapturePermission::Capture, "a");
    let read = grant(CapturePermission::Read, "a");
    let status = manager
        .start(&control, filter("a"), CaptureLevel::Metadata, None)
        .unwrap();
    let first = manager.lease(&read, status.id, false).unwrap();
    let second = manager.lease(&read, status.id, false).unwrap();
    assert_eq!(
        manager.lease(&read, status.id, false).unwrap_err(),
        CaptureError::Capacity
    );
    drop(second);
    let second = manager.lease(&read, status.id, false).unwrap();
    let export = manager.lease(&read, status.id, true).unwrap();
    let export2 = manager.lease(&read, status.id, true).unwrap();
    assert_eq!(
        manager.lease(&read, status.id, true).unwrap_err(),
        CaptureError::Capacity
    );
    manager.stop(&control, status.id).unwrap();
    assert!(!first.permits(&filter("a")));
    assert!(!second.permits(&filter("a")));
    assert!(export.permits(&filter("a")));
    assert_eq!(
        manager
            .start(&control, filter("a"), CaptureLevel::Metadata, None)
            .unwrap_err(),
        CaptureError::Conflict
    );
    drop(export);
    drop(export2);
    assert!(manager.wake_after(status.id).is_none());
    let next = manager
        .start(&control, filter("a"), CaptureLevel::Metadata, None)
        .unwrap();
    assert_ne!(next.id, status.id);
    assert_eq!(manager.stop(&control, status.id), Err(CaptureError::Gone));
    assert!(!first.permits(&filter("a")));
}
#[test]
fn expiry_releases_state_without_a_browser_request() {
    let manager = CaptureManager::new(true);
    manager
        .start(
            &grant(CapturePermission::Capture, "a"),
            filter("a"),
            CaptureLevel::Metadata,
            Some(Duration::from_millis(1)),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        // Inspect raw state: no public method gets a chance to perform lazy cleanup.
        if manager.state.lock().unwrap().session.is_none() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "server expiry worker did not release capture"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn abandoned_export_has_absolute_deadline_and_cannot_pin_new_captures() {
    let manager = CaptureManager::new(true);
    let control = grant(CapturePermission::Capture, "a");
    let read = grant(CapturePermission::Read, "a");
    let session = manager
        .start(&control, filter("a"), CaptureLevel::Metadata, None)
        .unwrap();
    let export = manager.lease(&read, session.id, true).unwrap();
    manager.stop(&control, session.id).unwrap();
    let mut state = manager.state.lock().unwrap();
    let deadline = state.session.as_ref().unwrap().exports[0].1;
    assert!(deadline <= Instant::now() + MAX_CAPTURE_EXPORT_DURATION);
    reap(&mut state, deadline);
    assert!(state.session.is_none());
    drop(state);
    assert!(!export.permits(&filter("a")));
}

#[test]
fn instrumentation_never_waits_for_capture_control_lock() {
    let manager = CaptureManager::new(true);
    manager
        .start(
            &grant(CapturePermission::Capture, "a"),
            filter("a"),
            CaptureLevel::Metadata,
            None,
        )
        .unwrap();
    assert!(manager.try_accepts(&filter("a"), CaptureLevel::Metadata));
    let owner = manager.state.lock().unwrap();
    assert!(!manager.try_accepts(&filter("a"), CaptureLevel::Metadata));
    drop(owner);
    assert!(manager.try_accepts(&filter("a"), CaptureLevel::Metadata));
}
