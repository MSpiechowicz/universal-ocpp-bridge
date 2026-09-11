use super::*;
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

type Next = Pin<Box<dyn Future<Output = Result<Option<ReportFragment>, ReportFailure>> + Send>>;

struct SourceGuard(Arc<AtomicBool>);
impl Drop for SourceGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test(start_paused = true)]
async fn missing_final_fragment_times_out_and_drops_source_and_all_reservations() {
    let budget = budget();
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = SourceGuard(dropped.clone());
    let mut first = Some(fragment(0, true, &[b"prefix"]));
    let failed = collect_report(
        key(),
        limits(),
        budget.clone(),
        move || -> Next {
            let _keep_source_registered = &guard;
            if let Some(first) = first.take() {
                Box::pin(future::ready(Ok(Some(first))))
            } else {
                Box::pin(future::pending())
            }
        },
        future::pending(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(failed.reason, ReportFailure::TimedOut);
    assert_eq!(
        failed.progress,
        ReportProgress {
            fragments: 1,
            items: 1,
            bytes: 6
        }
    );
    assert!(dropped.load(Ordering::SeqCst));
    empty(&budget);
}

#[tokio::test(start_paused = true)]
async fn fragments_never_extend_absolute_deadline() {
    let budget = budget();
    let mut sequence = 0;
    let failed = collect_report(
        key(),
        ReportLimits {
            timeout: Duration::from_secs(10),
            ..limits()
        },
        budget.clone(),
        move || {
            let current = sequence;
            sequence += 1;
            async move {
                tokio::time::sleep(Duration::from_secs(4)).await;
                Ok(Some(fragment(current, current < 2, &[b"item"])))
            }
        },
        future::pending(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(failed.reason, ReportFailure::TimedOut);
    assert_eq!(failed.progress.fragments, 2);
    empty(&budget);
}

#[tokio::test(start_paused = true)]
async fn explicit_cancellation_and_dropping_collection_release_source_without_background_work() {
    let budget = budget();
    let failed = collect_report(
        key(),
        limits(),
        budget.clone(),
        future::pending,
        tokio::time::sleep(Duration::from_millis(1)),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(failed.reason, ReportFailure::Cancelled);
    empty(&budget);

    let dropped = Arc::new(AtomicBool::new(false));
    let guard = SourceGuard(dropped.clone());
    let mut first = Some(fragment(0, true, &[b"prefix"]));
    let mut collection = Box::pin(collect_report(
        key(),
        limits(),
        budget.clone(),
        move || -> Next {
            let _keep_source_registered = &guard;
            match first.take() {
                Some(first) => Box::pin(future::ready(Ok(Some(first)))),
                None => Box::pin(future::pending()),
            }
        },
        future::pending(),
    ));
    assert!(futures::poll!(&mut collection).is_pending());
    assert_eq!(budget.snapshot().queues.multipart_assemblies, 1);
    drop(collection);
    assert!(dropped.load(Ordering::SeqCst));
    empty(&budget);
}

#[tokio::test]
async fn disconnect_and_decoder_errors_return_partial_counts_not_success() {
    for decoder_error in [false, true] {
        let budget = budget();
        let mut first = Some(fragment(0, true, &[b"prefix"]));
        let failed = collect_report(
            key(),
            limits(),
            budget.clone(),
            move || {
                future::ready(match first.take() {
                    Some(first) => Ok(Some(first)),
                    None if decoder_error => Err(ReportFailure::InvalidFragment),
                    None => Ok(None),
                })
            },
            future::pending(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(
            failed.reason,
            if decoder_error {
                ReportFailure::InvalidFragment
            } else {
                ReportFailure::Disconnected
            }
        );
        assert_eq!(failed.progress.fragments, 1);
        empty(&budget);
    }
}

#[tokio::test]
async fn ready_report_flood_yields_to_other_runtime_work() {
    let budget = budget();
    let progressed = Arc::new(AtomicBool::new(false));
    let observer = progressed.clone();
    let mut sequence = 0;
    let collector = collect_report(
        key(),
        ReportLimits {
            maximum_fragments: 1000,
            ..limits()
        },
        budget.clone(),
        move || {
            if sequence == 999 {
                assert!(observer.load(Ordering::SeqCst), "charging task was starved");
            }
            let fragment = fragment(sequence, sequence < 999, &[]);
            sequence += 1;
            future::ready(Ok(Some(fragment)))
        },
        future::pending(),
    );
    let (report, ()) = tokio::join!(collector, async move {
        tokio::task::yield_now().await;
        progressed.store(true, Ordering::SeqCst);
    });
    drop(report.unwrap());
    empty(&budget);
}

#[tokio::test]
async fn growing_report_releases_existing_prefix_when_other_work_exhausts_budget() {
    let budget = budget();
    let shared = budget.clone();
    let mut occupying_work = None;
    let mut sequence = 0;
    let failed = collect_report(
        key(),
        limits(),
        budget.clone(),
        move || {
            if sequence == 1 {
                let remaining = shared.limits().aggregate_queued_payload_bytes
                    - shared.limits().reserved_critical_payload_bytes
                    - shared.snapshot().queued_payload_bytes;
                occupying_work = Some(
                    shared
                        .try_reserve(WorkClass::DatabaseWork, remaining)
                        .unwrap(),
                );
            }
            let _keep_competing_work_alive = &occupying_work;
            let fragment = fragment(sequence, sequence == 0, &[b"payload"]);
            sequence += 1;
            future::ready(Ok(Some(fragment)))
        },
        future::pending(),
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(failed.reason, ReportFailure::Capacity(_)));
    assert_eq!(failed.progress.fragments, 1);
    empty(&budget);
}
