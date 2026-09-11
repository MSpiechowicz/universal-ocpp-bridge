use super::*;
use crate::{
    DiagnosticBoundary, DiagnosticObservation, DiagnosticOutcome, DiagnosticSummary,
    DiagnosticTraceContext, WorkClass,
};
use uob_contracts::{
    BridgeId, ProcessInstanceId, TraceDirection, TraceId, TraceSequence, TraceStage, UtcTimestamp,
};

fn setup(manager: CaptureManager) -> (CaptureManager, CaptureGrant, CaptureFilter, u64) {
    let filter = CaptureFilter {
        bridge: BridgeId::new("bridge").unwrap(),
        station: None,
        target: None,
    };
    let grant = CaptureGrant::new(
        filter.bridge.clone(),
        vec![CapturePermission::Capture, CapturePermission::Read],
        None,
        None,
    )
    .unwrap();
    let id = manager
        .start(&grant, filter.clone(), CaptureLevel::Metadata, None)
        .unwrap()
        .id;
    (manager, grant, filter, id)
}

fn record(sequence: u64) -> SanitizedDiagnostic {
    DiagnosticBoundary
        .serialize(
            DiagnosticTraceContext {
                trace_id: TraceId::new(format!("process:{sequence}")).unwrap(),
                process_instance_id: ProcessInstanceId::new("process").unwrap(),
                trace_sequence: TraceSequence(sequence),
                target: None,
                correlation_id: None,
                parent_trace_id: None,
                stage: TraceStage::new("test.stage").unwrap(),
                direction: TraceDirection::Internal,
                observed_at: UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
                duration_micros: None,
                outcome: DiagnosticOutcome::Succeeded,
            },
            DiagnosticObservation {
                summary: DiagnosticSummary::OperationCompleted,
                attributes: vec![],
            },
        )
        .unwrap()
}

fn emit(manager: &CaptureManager, filter: &CaptureFilter) {
    assert!(manager.try_record(filter, |sequence, _| Some(record(sequence))));
}

#[test]
fn two_readers_share_one_bounded_ring_and_overflow_evidence() {
    let (manager, grant, filter, id) =
        setup(CaptureManager::with_ring_limits(true, 4096, 2).unwrap());
    emit(&manager, &filter);
    let first = manager.lease(&grant, id, false).unwrap();
    let second = manager.lease(&grant, id, false).unwrap();
    let a = first.read_after(None).unwrap().record.unwrap();
    let b = second.read_after(None).unwrap().record.unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(manager.resources.snapshot().queues.capture_records, 1);
    drop((a, b));
    emit(&manager, &filter);
    emit(&manager, &filter);
    let read = first.read_after(None).unwrap();
    assert_eq!(read.record.unwrap().sequence, 1);
    assert_eq!(read.window.first_sequence, Some(1));
    assert_eq!(read.window.next_sequence, 3);
    assert_eq!(read.window.evicted_records, 1);
    assert_eq!(read.window.retained_records, 2);
    assert!(read.window.retained_bytes <= 4096);
    assert_eq!(
        second.read_after(Some(1)).unwrap().record.unwrap().sequence,
        2
    );
    assert!(second.read_after(Some(2)).unwrap().record.is_none());
    assert_eq!(manager.resources.snapshot().queues.capture_records, 2);
}

#[test]
fn byte_cap_and_lower_only_configuration_cannot_be_bypassed() {
    for (bytes, records) in [(0, 1), (8 * 1024 * 1024 + 1, 1), (1, 0), (1, 2001)] {
        assert!(CaptureManager::with_ring_limits(true, bytes, records).is_err());
    }
    let bytes = record(0).encoded_json().len();
    let (manager, grant, filter, id) =
        setup(CaptureManager::with_ring_limits(true, bytes * 2, 2000).unwrap());
    for _ in 0..3 {
        emit(&manager, &filter);
    }
    let lease = manager.lease(&grant, id, false).unwrap();
    let read = lease.read_after(None).unwrap();
    assert_eq!(read.window.retained_records, 2);
    assert_eq!(read.window.retained_bytes, bytes * 2);
    assert_eq!(read.window.evicted_records, 1);
    drop(read);
    let (tiny, grant, filter, id) =
        setup(CaptureManager::with_ring_limits(true, bytes - 1, 1).unwrap());
    assert!(!tiny.try_record(&filter, |sequence, _| Some(record(sequence))));
    let read = tiny
        .lease(&grant, id, false)
        .unwrap()
        .read_after(None)
        .unwrap();
    assert_eq!(read.window.retained_bytes, 0);
    assert_eq!(read.window.dropped_records, 1);
    assert_eq!(read.window.next_sequence, 1);
}

#[test]
fn filtering_contention_and_failed_formatting_are_bounded_and_visible() {
    let (manager, grant, filter, id) = setup(CaptureManager::new(true));
    let mut other = filter.clone();
    other.bridge = BridgeId::new("other").unwrap();
    assert!(!manager.try_record(&other, |_, _| panic!("filtered work formatted")));
    let guard = manager.state.lock().unwrap();
    assert!(!manager.try_record(&filter, |_, _| panic!("contended work formatted")));
    drop(guard);
    assert!(!manager.try_record(&filter, |_, _| None));
    emit(&manager, &filter);
    let lease = manager.lease(&grant, id, false).unwrap();
    let read = lease.read_after(None).unwrap();
    assert_eq!(read.record.unwrap().sequence, 1);
    assert_eq!(read.window.next_sequence, 2);
    assert_eq!(read.window.dropped_records, 2);
    assert_eq!(manager.resources.snapshot().dropped_diagnostics, 2);
    manager.stop(&grant, id).unwrap();
    assert!(!manager.try_record(&filter, |_, _| panic!("stopped work formatted")));
}

#[test]
fn pinned_readers_remain_accounted_and_cannot_expand_the_shared_budget() {
    let mut limits = RuntimeResourceLimits::default();
    limits.queues.capture_records = 2;
    let resources = RuntimeResourceBudget::new(limits).unwrap();
    let (manager, grant, filter, id) =
        setup(CaptureManager::with_resources(true, resources.clone()));
    emit(&manager, &filter);
    let lease = manager.lease(&grant, id, false).unwrap();
    let pinned = lease.read_after(None).unwrap().record.unwrap();
    emit(&manager, &filter);
    assert!(!manager.try_record(&filter, |sequence, _| Some(record(sequence))));
    let read = lease.read_after(None).unwrap();
    assert_eq!(read.window.retained_records, 1);
    assert_eq!(read.window.evicted_records, 1);
    assert_eq!(read.window.dropped_records, 1);
    assert_eq!(resources.snapshot().queues.capture_records, 2);
    drop((pinned, read));
    emit(&manager, &filter);
    assert_eq!(resources.snapshot().queues.capture_records, 2);
    manager.stop(&grant, id).unwrap();
    assert_eq!(resources.snapshot().trace_ring_bytes, 0);
}

#[test]
fn capture_expiry_revokes_live_reads_but_only_bounded_exports_retain_memory() {
    let (manager, grant, filter, id) = setup(CaptureManager::new(true));
    emit(&manager, &filter);
    let live = manager.lease(&grant, id, false).unwrap();
    let export = manager.lease(&grant, id, true).unwrap();
    manager
        .state
        .lock()
        .unwrap()
        .session
        .as_mut()
        .unwrap()
        .deadline = Instant::now();
    manager.expire();
    assert!(matches!(live.read_after(None), Err(CaptureError::Gone)));
    assert!(export.read_after(None).unwrap().record.is_some());
    assert!(manager.resources.snapshot().trace_ring_bytes > 0);
    manager
        .state
        .lock()
        .unwrap()
        .session
        .as_mut()
        .unwrap()
        .exports[0]
        .1 = Instant::now();
    manager.expire();
    assert!(matches!(export.read_after(None), Err(CaptureError::Gone)));
    assert_eq!(manager.resources.snapshot().trace_ring_bytes, 0);
    let new = manager
        .start(&grant, filter.clone(), CaptureLevel::Metadata, None)
        .unwrap();
    emit(&manager, &filter);
    let read = manager
        .lease(&grant, new.id, false)
        .unwrap()
        .read_after(None)
        .unwrap();
    assert_eq!(read.record.unwrap().sequence, 1);
    assert_eq!(read.window.evicted_records, 0);
    assert_eq!(read.window.dropped_records, 0);
    assert!(matches!(live.read_after(None), Err(CaptureError::Gone)));
}

#[test]
fn pressure_sheds_optional_details_and_preserves_critical_capacity() {
    let limits = RuntimeResourceLimits {
        aggregate_queued_payload_bytes: 8192,
        reserved_critical_payload_bytes: 1024,
        trace_ring_bytes: 4096,
        ..RuntimeResourceLimits::default()
    };
    let resources = RuntimeResourceBudget::new(limits).unwrap();
    let (manager, grant, filter, id) =
        setup(CaptureManager::with_resources(true, resources.clone()));
    let pressure = resources
        .try_reserve(WorkClass::TargetIngress, 6000)
        .unwrap();
    assert!(manager.try_record(&filter, |sequence, shed| {
        assert!(shed);
        Some(record(sequence))
    }));
    let read = manager
        .lease(&grant, id, false)
        .unwrap()
        .read_after(None)
        .unwrap();
    assert_eq!(read.window.shed_records, 1);
    let critical = resources
        .try_reserve(WorkClass::ChargerRequest, 1024)
        .unwrap();
    assert!(resources.snapshot().queued_payload_bytes <= 8192);
    drop((critical, pressure, read));
    manager.stop(&grant, id).unwrap();
    assert_eq!(resources.snapshot().queued_payload_bytes, 0);
}

#[test]
fn ring_watermark_requests_shedding_before_overflow() {
    let (manager, grant, filter, id) =
        setup(CaptureManager::with_ring_limits(true, 4096, 4).unwrap());
    for _ in 0..3 {
        assert!(manager.try_record(&filter, |sequence, shed| {
            assert!(!shed);
            Some(record(sequence))
        }));
    }
    assert!(manager.try_record(&filter, |sequence, shed| {
        assert!(shed);
        Some(record(sequence))
    }));
    let read = manager
        .lease(&grant, id, false)
        .unwrap()
        .read_after(None)
        .unwrap();
    assert_eq!(read.window.shed_records, 1);
    assert_eq!(read.window.evicted_records, 0);
}

#[test]
fn simultaneous_producers_cannot_reorder_process_sequences() {
    let (manager, grant, filter, id) = setup(CaptureManager::new(true));
    let accepted = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..200 {
                    if manager.try_record(&filter, |sequence, _| Some(record(sequence))) {
                        accepted.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    let lease = manager.lease(&grant, id, false).unwrap();
    let mut after = None;
    let mut seen = 0;
    loop {
        let read = lease.read_after(after).unwrap();
        let Some(record) = read.record else {
            assert_eq!(read.window.next_sequence, seen);
            assert_eq!(read.window.dropped_records, 800 - seen);
            break;
        };
        assert_eq!(record.sequence, seen);
        let encoded: uob_contracts::TraceRecord =
            serde_json::from_slice(record.diagnostic.encoded_json()).unwrap();
        assert_eq!(encoded.trace_sequence.0, seen);
        after = Some(seen);
        seen += 1;
    }
    assert_eq!(
        usize::try_from(seen).unwrap(),
        accepted.load(Ordering::Relaxed)
    );
    assert!(seen > 0);
}

#[test]
fn pinned_old_capture_cannot_bypass_the_same_service_ring_cap() {
    let (manager, grant, filter, id) =
        setup(CaptureManager::with_ring_limits(true, 4096, 1).unwrap());
    emit(&manager, &filter);
    let pinned = manager
        .lease(&grant, id, false)
        .unwrap()
        .read_after(None)
        .unwrap()
        .record
        .unwrap();
    manager.stop(&grant, id).unwrap();
    let next = manager
        .start(&grant, filter.clone(), CaptureLevel::Metadata, None)
        .unwrap();
    assert!(!manager.try_record(&filter, |sequence, _| Some(record(sequence))));
    let lease = manager.lease(&grant, next.id, false).unwrap();
    let read = lease.read_after(None).unwrap();
    assert!(read.record.is_none());
    assert_eq!(read.window.dropped_records, 1);
    assert_eq!(manager.resources.snapshot().queues.capture_records, 1);
    drop(pinned);
    emit(&manager, &filter);
    assert_eq!(lease.read_after(None).unwrap().record.unwrap().sequence, 2);
    manager.stop(&grant, next.id).unwrap();
    assert_eq!(manager.resources.snapshot().trace_ring_bytes, 0);
}
