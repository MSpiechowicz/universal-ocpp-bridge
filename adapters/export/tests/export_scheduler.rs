#![cfg(target_os = "linux")]

#[path = "export_scheduler/support.rs"]
mod support;

#[path = "export_scheduler/boundaries.rs"]
mod boundaries;

#[path = "export_scheduler/contention.rs"]
mod contention;

use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use tokio::time::{Instant, advance};
use uob_application::{ExportSpool, RuntimeResourceBudget, RuntimeResourceLimits};
use uob_external_export_adapter::ExportScheduler;

use support::{
    Behavior, Control, Fixture, confirm_after_retries, populated_spool, resources, start, until,
};

#[tokio::test(start_paused = true)]
async fn disabled_selection_allocates_no_export_worker() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let resources = resources();
    let (spool, namespace) = populated_spool(&fixture, &resources).await;
    let disabled = uob_external_export_adapter::ValidatedDataExport::Disabled {
        discard_audit: None,
    };
    assert!(
        ExportScheduler::start(
            disabled,
            namespace.clone(),
            spool.clone(),
            resources.clone()
        )
        .unwrap()
        .is_none()
    );
    advance(Duration::from_secs(5)).await;
    assert_eq!(spool.status(namespace).await.unwrap().pending_records, 2);
    assert_eq!(resources.snapshot().queues.exporter_batches, 0);
}

#[tokio::test(start_paused = true)]
async fn failures_replay_same_claim_after_backoff_then_confirm_without_parallel_sessions() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let resources = resources();
    let (spool, namespace) = populated_spool(&fixture, &resources).await;
    let control = Arc::new(Control::default());
    let started = Instant::now();
    let handle = start(
        &control,
        Behavior::FailTwice,
        &namespace,
        &spool,
        &resources,
    );
    confirm_after_retries(&control, &handle).await;
    let observations = control.observations.lock().clone();
    assert_eq!(observations.len(), 3);
    assert!(observations[0].at.duration_since(started) <= Duration::from_secs(1));
    assert!(
        observations
            .iter()
            .all(|item| item.batch_id == observations[0].batch_id
                && item.records == 2
                && item.bytes <= 256 * 1024)
    );
    assert!(observations[1].at.duration_since(observations[0].at) >= Duration::from_millis(150));
    assert!(observations[2].at.duration_since(observations[1].at) >= Duration::from_millis(300));
    assert_eq!(control.peak.load(Ordering::SeqCst), 1);
    assert_eq!(handle.health().confirmed_records, 2);
    handle.shutdown().await.unwrap();
    assert_eq!(spool.status(namespace).await.unwrap().pending_records, 0);
    assert_eq!(resources.snapshot().queues.exporter_batches, 0);
}

#[tokio::test(start_paused = true)]
async fn aborted_session_reopens_exact_durable_claim() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let resources = resources();
    let (spool, namespace) = populated_spool(&fixture, &resources).await;
    let first = Arc::new(Control::default());
    let handle = start(&first, Behavior::Hung, &namespace, &spool, &resources);
    until(|| first.observations.lock().len() == 1).await;
    assert_eq!(
        first.attempts.load(Ordering::SeqCst),
        1,
        "{:?}",
        handle.health()
    );
    let old_id = first.observations.lock()[0].batch_id.clone();
    drop(handle);
    until(|| first.open.load(Ordering::SeqCst) == 0).await;
    drop(spool);

    let (_, reopened) = fixture.open(&resources);
    let reopened = Arc::new(reopened);
    let next = Arc::new(Control::default());
    let handle = start(
        &next,
        Behavior::FailTwice,
        &namespace,
        &reopened,
        &resources,
    );
    confirm_after_retries(&next, &handle).await;
    assert!(
        next.observations
            .lock()
            .iter()
            .all(|item| item.batch_id == old_id)
    );
    handle.shutdown().await.unwrap();
    assert_eq!(reopened.status(namespace).await.unwrap().pending_records, 0);
}

#[tokio::test(start_paused = true)]
async fn hung_provider_is_cancelled_without_accumulating_attempt_tasks() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let resources = resources();
    let (spool, namespace) = populated_spool(&fixture, &resources).await;
    let control = Arc::new(Control::default());
    let handle = start(&control, Behavior::Hung, &namespace, &spool, &resources);
    for attempt in 1..=3 {
        until(|| control.observations.lock().len() == attempt).await;
        advance(Duration::from_secs(6)).await;
        tokio::task::yield_now().await;
        advance(Duration::from_secs(3)).await;
        until(|| handle.health().retry_count == attempt as u64).await;
        advance(Duration::from_secs(3)).await;
    }
    assert!(
        control.attempts.load(Ordering::SeqCst) >= 3,
        "{:?}",
        handle.health()
    );
    assert_eq!(control.peak.load(Ordering::SeqCst), 1);
    assert_eq!(handle.health().confirmed_records, 0);
    let stopping = tokio::spawn(handle.shutdown());
    tokio::task::yield_now().await;
    advance(Duration::from_secs(3)).await;
    stopping.await.unwrap().unwrap();
    assert_eq!(spool.status(namespace).await.unwrap().pending_records, 2);
    assert_eq!(control.open.load(Ordering::SeqCst), 0);
    assert_eq!(resources.snapshot().queues.exporter_batches, 0);
}

#[tokio::test(start_paused = true)]
async fn small_batch_fits_a_budget_below_maximum_batch_bytes() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let resources = RuntimeResourceBudget::new(RuntimeResourceLimits {
        aggregate_queued_payload_bytes: 192 * 1024,
        reserved_critical_payload_bytes: 64 * 1024,
        trace_ring_bytes: 64 * 1024,
        ..RuntimeResourceLimits::default()
    })
    .unwrap();
    let (spool, namespace) = populated_spool(&fixture, &resources).await;
    let control = Arc::new(Control::default());
    let handle = start(
        &control,
        Behavior::FailTwice,
        &namespace,
        &spool,
        &resources,
    );
    confirm_after_retries(&control, &handle).await;
    handle.shutdown().await.unwrap();
    assert_eq!(spool.status(namespace).await.unwrap().pending_records, 0);
    assert_eq!(resources.snapshot().queues.exporter_batches, 0);
}

#[tokio::test(start_paused = true)]
async fn confirmed_report_survives_provider_shutdown_timeout_without_remote_replay() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let resources = resources();
    let (spool, namespace) = populated_spool(&fixture, &resources).await;
    let control = Arc::new(Control::default());
    let handle = start(
        &control,
        Behavior::CommitSlowShutdown,
        &namespace,
        &spool,
        &resources,
    );
    until(|| control.shutdown_seen.load(Ordering::SeqCst) == 1).await;
    advance(Duration::from_secs(2)).await;
    until(|| handle.health().confirmed_records == 2).await;
    assert_eq!(
        handle.health().provider.state,
        uob_application::DatabaseHealthState::Degraded
    );
    assert_eq!(
        handle.health().provider.reason.as_deref(),
        Some("provider.shutdown_timeout")
    );
    assert_eq!(control.open.load(Ordering::SeqCst), 0);
    assert_eq!(control.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(handle.health().confirmed_records, 2);
    handle.shutdown().await.unwrap();
    assert_eq!(
        spool
            .status(namespace.clone())
            .await
            .unwrap()
            .pending_records,
        0
    );
    assert_eq!(resources.snapshot().queues.exporter_batches, 0);
}

#[tokio::test(start_paused = true)]
async fn real_claim_memory_pressure_recovers_without_provider_replay() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let resources = resources();
    let (spool, namespace) = populated_spool(&fixture, &resources).await;
    assert_eq!(resources.snapshot().queued_payload_bytes, 0);
    let available = resources.limits().aggregate_queued_payload_bytes
        - resources.limits().reserved_critical_payload_bytes;
    let reservation = resources
        .try_reserve(uob_application::WorkClass::DatabaseWork, available - 100)
        .unwrap();
    let control = Arc::new(Control::default());
    let handle = start(&control, Behavior::Commit, &namespace, &spool, &resources);
    until(|| handle.health().retry_count > 0).await;
    assert_eq!(
        handle.health().provider.reason.as_deref(),
        Some("spool.memory_pressure")
    );
    assert_eq!(control.attempts.load(Ordering::SeqCst), 0);
    drop(reservation);
    tokio::task::yield_now().await;
    advance(Duration::from_millis(250)).await;
    until(|| handle.health().confirmed_records == 2).await;
    assert_eq!(control.attempts.load(Ordering::SeqCst), 1);
    handle.shutdown().await.unwrap();
    assert_eq!(spool.status(namespace).await.unwrap().pending_records, 0);
}
