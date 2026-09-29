use std::{
    collections::BTreeSet,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use tokio::time::Instant;
use uob_application::{ExportSpool, ExportSpoolErrorCode, PageLimit};

use super::support::{
    Behavior, Control, Fixture, populated_records, record, resources, start, until,
};

#[tokio::test(start_paused = true)]
async fn one_hundred_one_pending_records_are_confirmed_without_loss() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let resources = resources();
    let expected: BTreeSet<_> = (0..101).map(|n| format!("critical-{n:03}")).collect();
    let (spool, namespace) = populated_records(
        &fixture,
        &resources,
        expected.iter().map(|id| record(id)).collect(),
    )
    .await;
    assert!(
        PageLimit::new(101).is_err(),
        "the page API rejects more than 100"
    );
    let claim = spool
        .claim_delivery(namespace.clone(), PageLimit::new(100).unwrap(), 256 * 1024)
        .await
        .unwrap()
        .unwrap();
    assert!((1..=100).contains(&claim.items.len()));
    drop(claim);
    let control = Arc::new(Control::default());
    let handle = start(&control, Behavior::Commit, &namespace, &spool, &resources);
    until(|| handle.health().confirmed_records == 101).await;
    let observed = control.observations.lock().clone();
    assert!(observed.len() >= 2, "the 101st record must not be omitted");
    assert!(
        observed
            .iter()
            .all(|batch| (1..=100).contains(&batch.records))
    );
    assert!(
        observed
            .windows(2)
            .all(|pair| pair[0].batch_id != pair[1].batch_id)
    );
    assert!(observed.iter().all(|batch| batch.bytes <= 256 * 1024));
    let delivered: Vec<_> = observed
        .into_iter()
        .flat_map(|batch| batch.identities)
        .collect();
    assert_eq!(delivered.len(), 101);
    assert_eq!(delivered.into_iter().collect::<BTreeSet<_>>(), expected);
    handle.shutdown().await.unwrap();
    assert_eq!(spool.status(namespace).await.unwrap().pending_records, 0);
}

#[tokio::test(start_paused = true)]
async fn a_single_pending_record_flushes_within_one_second() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let resources = resources();
    let (spool, namespace) =
        populated_records(&fixture, &resources, vec![record("low-load")]).await;
    let control = Arc::new(Control::default());
    let started = Instant::now();
    let handle = start(&control, Behavior::Commit, &namespace, &spool, &resources);
    until(|| handle.health().confirmed_records == 1).await;
    let observed = control.observations.lock().clone();
    assert_eq!(observed[0].identities, ["low-load"]);
    assert!(observed[0].at.duration_since(started) <= Duration::from_secs(1));
    handle.shutdown().await.unwrap();
    assert_eq!(spool.status(namespace).await.unwrap().pending_records, 0);
}

#[tokio::test(start_paused = true)]
async fn conservative_byte_claim_admits_safe_batch_and_retains_oversized_record() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let resources = resources();
    let admitted = format!("near-limit-{}", "x".repeat(19_000));
    let (spool, namespace) = populated_records(&fixture, &resources, vec![record(&admitted)]).await;
    let control = Arc::new(Control::default());
    let handle = start(&control, Behavior::Commit, &namespace, &spool, &resources);
    until(|| handle.health().confirmed_records == 1).await;
    let observed = control.observations.lock().clone();
    assert_eq!(observed[0].identities, [admitted]);
    assert!(
        observed[0].bytes <= 256 * 1024,
        "encoded batch exceeded scheduler limit"
    );
    handle.shutdown().await.unwrap();
    assert_eq!(spool.status(namespace).await.unwrap().pending_records, 0);

    let Some(fixture) = Fixture::new() else {
        return;
    };
    let over = format!("over-limit-{}", "x".repeat(23_000));
    let (spool, namespace) = populated_records(&fixture, &resources, vec![record(&over)]).await;
    assert_eq!(
        spool
            .claim_delivery(namespace.clone(), PageLimit::new(100).unwrap(), 256 * 1024)
            .await
            .unwrap_err()
            .code(),
        ExportSpoolErrorCode::DeliveryLimitExceeded
    );
    let control = Arc::new(Control::default());
    let handle = start(&control, Behavior::Commit, &namespace, &spool, &resources);
    until(|| {
        handle.health().provider.reason.is_some() || control.attempts.load(Ordering::SeqCst) > 0
    })
    .await;
    assert_eq!(
        handle.health().provider.reason.as_deref(),
        Some("spool.failure"),
        "{:?}",
        handle.health()
    );
    assert_eq!(control.attempts.load(Ordering::SeqCst), 0);
    assert_eq!(
        spool
            .status(namespace.clone())
            .await
            .unwrap()
            .pending_records,
        1
    );
    assert!(
        handle.shutdown().await.is_err(),
        "oversized claim must be surfaced"
    );
    assert_eq!(spool.status(namespace).await.unwrap().pending_records, 1);
}

#[tokio::test(start_paused = true)]
async fn provider_record_and_byte_limits_bound_new_durable_claims() {
    for limits in [(1, 256 * 1024), (100, 10 * 1024)] {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let resources = resources();
        let (spool, namespace) = populated_records(
            &fixture,
            &resources,
            vec![record("first"), record("second")],
        )
        .await;
        let control = Arc::new(Control::default());
        *control.limits.lock() = Some(limits);
        let handle = start(&control, Behavior::Commit, &namespace, &spool, &resources);
        until(|| handle.health().confirmed_records == 2).await;
        let observations = control.observations.lock().clone();
        assert_eq!(observations.len(), 2, "limits: {limits:?}");
        assert!(
            observations
                .iter()
                .all(|batch| batch.records == 1 && batch.bytes <= limits.1)
        );
        assert_ne!(observations[0].batch_id, observations[1].batch_id);
        handle.shutdown().await.unwrap();
        assert_eq!(spool.status(namespace).await.unwrap().pending_records, 0);
    }
}

#[tokio::test(start_paused = true)]
async fn older_oversized_claim_is_reported_without_remote_confirmation() {
    for limits in [(1, 256 * 1024), (100, 10 * 1024)] {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let resources = resources();
        let (spool, namespace) = populated_records(
            &fixture,
            &resources,
            vec![record("first"), record("second")],
        )
        .await;
        let pinned = spool
            .claim_delivery(namespace.clone(), PageLimit::new(100).unwrap(), 256 * 1024)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pinned.items.len(), 2);
        let control = Arc::new(Control::default());
        *control.limits.lock() = Some(limits);
        let handle = start(&control, Behavior::Commit, &namespace, &spool, &resources);
        until(|| handle.health().provider.reason.as_deref() == Some("spool.failure")).await;
        assert_eq!(control.attempts.load(Ordering::SeqCst), 0);
        assert_eq!(handle.health().confirmed_records, 0);
        assert!(matches!(
            handle.shutdown().await,
            Err(uob_external_export_adapter::ExportSchedulerError::Spool(error))
                if error.code() == ExportSpoolErrorCode::DeliveryLimitExceeded
        ));
        let still_pinned = spool
            .claim_delivery(namespace.clone(), PageLimit::new(100).unwrap(), 256 * 1024)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(still_pinned.batch_id, pinned.batch_id);
        assert_eq!(spool.status(namespace).await.unwrap().pending_records, 2);
    }
}
