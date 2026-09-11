use std::{sync::mpsc, thread, time::Duration};

use super::*;

fn budget() -> RuntimeResourceBudget {
    RuntimeResourceBudget::new(RuntimeResourceLimits {
        aggregate_queued_payload_bytes: 100,
        reserved_critical_payload_bytes: 20,
        trace_ring_bytes: 40,
        queues: RuntimeQueueLimits {
            diagnostics: 2,
            capture_records: 2,
            ..RuntimeQueueLimits::default()
        },
        ..RuntimeResourceLimits::default()
    })
    .expect("valid budget")
}

#[test]
fn optional_admission_pressure_release_and_drop_metrics_never_wait_for_usage_lock() {
    let budget = budget();
    let reservation = budget
        .try_reserve_diagnostic(WorkClass::CaptureTrace, 20)
        .expect("capture reservation");
    let usage = budget.inner.usage.lock().expect("hold budget lock");
    let worker_budget = budget.clone();
    let (completed_tx, completed_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let rejected = worker_budget
            .try_reserve_diagnostic(WorkClass::CaptureTrace, 1)
            .is_none();
        let pressure = worker_budget.try_diagnostic_pressure();
        drop(reservation);
        worker_budget.record_diagnostic_drop(DiagnosticDropReason::Full);
        completed_tx.send((rejected, pressure)).expect("result");
    });

    let completed = completed_rx.recv_timeout(Duration::from_secs(1));
    drop(usage);
    worker.join().expect("producer completed");

    assert_eq!(completed.expect("producer must not wait"), (true, true));
    let snapshot = budget.snapshot();
    assert_eq!(snapshot.queued_payload_bytes, 0);
    assert_eq!(snapshot.trace_ring_bytes, 0);
    assert_eq!(snapshot.queues.capture_records, 0);
    assert_eq!(snapshot.dropped_diagnostics, 1);
}

#[test]
fn optional_admission_applies_item_trace_and_shared_bounds_and_preserves_critical_capacity() {
    let budget = budget();
    let first = budget
        .try_reserve_diagnostic(WorkClass::CaptureTrace, 20)
        .expect("first capture");
    let second = budget
        .try_reserve_diagnostic(WorkClass::CaptureTrace, 20)
        .expect("second capture");
    assert!(
        budget
            .try_reserve_diagnostic(WorkClass::CaptureTrace, 0)
            .is_none()
    );
    drop(first);
    assert!(
        budget
            .try_reserve_diagnostic(WorkClass::CaptureTrace, 21)
            .is_none()
    );
    let replacement = budget
        .try_reserve_diagnostic(WorkClass::CaptureTrace, 20)
        .expect("released capture capacity reused");
    let diagnostic = budget
        .try_reserve_diagnostic(WorkClass::Diagnostic, 40)
        .expect("remaining optional bytes");
    assert!(
        budget
            .try_reserve_diagnostic(WorkClass::Diagnostic, 1)
            .is_none()
    );

    let critical = budget
        .try_reserve(WorkClass::CriticalReport, 20)
        .expect("critical report retains reserved capacity");
    let snapshot = budget.snapshot();
    assert_eq!(snapshot.queued_payload_bytes, 100);
    assert_eq!(snapshot.trace_ring_bytes, 40);
    assert_eq!(snapshot.queues.capture_records, 2);
    assert_eq!(snapshot.queues.diagnostics, 1);

    drop((second, replacement, diagnostic, critical));
    let snapshot = budget.snapshot();
    assert_eq!(snapshot.queued_payload_bytes, 0);
    assert_eq!(snapshot.trace_ring_bytes, 0);
    assert_eq!(snapshot.queues.capture_records, 0);
    assert_eq!(snapshot.queues.diagnostics, 0);
}

#[test]
fn optional_admission_rejects_all_other_work_classes() {
    let budget = budget();
    for class in [
        WorkClass::ChargerRequest,
        WorkClass::DatabaseWork,
        WorkClass::Subscriber,
        WorkClass::PendingRequest,
        WorkClass::MultipartAssembly,
        WorkClass::TargetIngress,
        WorkClass::TargetEgress,
        WorkClass::TargetRetry,
        WorkClass::CriticalReport,
        WorkClass::ExporterBatch,
    ] {
        assert!(budget.try_reserve_diagnostic(class, 1).is_none());
    }
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
}

#[test]
fn pressure_uses_three_quarters_of_noncritical_allowance_and_clears_after_release() {
    let budget = budget();
    let first = budget
        .try_reserve_diagnostic(WorkClass::Diagnostic, 59)
        .expect("below threshold");
    assert!(!budget.try_diagnostic_pressure());
    let second = budget
        .try_reserve_diagnostic(WorkClass::Diagnostic, 1)
        .expect("at threshold");
    assert!(budget.try_diagnostic_pressure());
    drop(second);
    assert!(!budget.try_diagnostic_pressure());
    drop(first);
}

#[test]
fn optional_byte_accounting_rejects_integer_overflow() {
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits {
        aggregate_queued_payload_bytes: usize::MAX,
        reserved_critical_payload_bytes: 0,
        ..RuntimeResourceLimits::default()
    })
    .expect("valid maximum byte cap");
    let _first = budget
        .try_reserve_diagnostic(WorkClass::Diagnostic, 1)
        .expect("one byte");
    assert!(
        budget
            .try_reserve_diagnostic(WorkClass::Diagnostic, usize::MAX)
            .is_none()
    );
    assert_eq!(budget.snapshot().queued_payload_bytes, 1);
}

#[test]
fn concurrent_optional_release_and_admission_keep_accounting_bounded() {
    let budget = budget();
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let budget = budget.clone();
            thread::spawn(move || {
                for _ in 0..1_000 {
                    let diagnostic = budget.try_reserve_diagnostic(WorkClass::Diagnostic, 20);
                    let capture = budget.try_reserve_diagnostic(WorkClass::CaptureTrace, 20);
                    let snapshot = budget.snapshot();
                    assert!(snapshot.queued_payload_bytes <= 80);
                    assert!(snapshot.trace_ring_bytes <= 40);
                    assert!(snapshot.queues.capture_records <= 2);
                    assert!(snapshot.queues.diagnostics <= 2);
                    drop((diagnostic, capture));
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("producer");
    }

    let snapshot = budget.snapshot();
    assert_eq!(snapshot.queued_payload_bytes, 0);
    assert_eq!(snapshot.trace_ring_bytes, 0);
    assert_eq!(snapshot.queues.capture_records, 0);
    assert_eq!(snapshot.queues.diagnostics, 0);
}
