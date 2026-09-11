use std::{collections::VecDeque, future, time::Duration};
use uob_application::{RuntimeResourceBudget, RuntimeResourceLimits, WorkClass};
use uob_contracts::{CorrelationId, ProtocolActionName, ProtocolEdition, StationId};
use uob_protocol_adapter::multipart::*;

#[path = "multipart/async_cases.rs"]
mod async_cases;

fn key() -> ReportKey {
    ReportKey {
        station: StationId::new("station-1").unwrap(),
        connection: CorrelationId::new("socket-1").unwrap(),
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new("NotifyReport").unwrap(),
        request_id: 42,
        correlation: CorrelationId::new("request-42").unwrap(),
    }
}

fn budget() -> RuntimeResourceBudget {
    RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap()
}

fn limits() -> ReportLimits {
    ReportLimits {
        maximum_bytes: 32,
        maximum_items: 4,
        maximum_fragments: 3,
        timeout: Duration::from_secs(1),
    }
}

fn fragment(sequence: u32, more: bool, items: &[&[u8]]) -> ReportFragment {
    ReportFragment {
        key: key(),
        sequence,
        more,
        items: items.iter().map(|item| item.to_vec()).collect(),
    }
}

async fn collect(
    fragments: Vec<ReportFragment>,
    limits: ReportLimits,
    budget: &RuntimeResourceBudget,
) -> Result<CollectedReport, Box<PartialReport>> {
    let mut fragments = VecDeque::from(fragments);
    collect_report(
        key(),
        limits,
        budget.clone(),
        move || future::ready(Ok(fragments.pop_front())),
        future::pending(),
    )
    .await
}

fn empty(budget: &RuntimeResourceBudget) {
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
    assert_eq!(budget.snapshot().queues.multipart_assemblies, 0);
}

#[tokio::test]
async fn completion_preserves_order_correlation_and_reserves_until_consumer_drop() {
    let budget = budget();
    let report = collect(
        vec![
            fragment(0, true, &[b"first", b"second"]),
            fragment(1, false, &[b"third"]),
        ],
        limits(),
        &budget,
    )
    .await
    .unwrap();
    assert_eq!(report.key(), &key());
    assert_eq!(
        report.items().collect::<Vec<_>>(),
        [b"first".as_slice(), b"second", b"third"]
    );
    assert_eq!(
        report.progress(),
        ReportProgress {
            fragments: 2,
            items: 3,
            bytes: 16
        }
    );
    assert_eq!(budget.snapshot().queues.multipart_assemblies, 1);
    assert!(budget.snapshot().queued_payload_bytes >= 16);
    drop(report);
    empty(&budget);
}

#[tokio::test]
async fn duplicate_conflicting_missing_and_out_of_order_sequences_fail_terminally() {
    for (sequence, payload, expected) in [
        (
            0,
            b"first".as_slice(),
            ReportFailure::DuplicateOrConflictingSequence,
        ),
        (
            0,
            b"conflict".as_slice(),
            ReportFailure::DuplicateOrConflictingSequence,
        ),
        (
            2,
            b"gap".as_slice(),
            ReportFailure::MissingOrOutOfOrderSequence,
        ),
        (
            u32::MAX,
            b"overflow".as_slice(),
            ReportFailure::MissingOrOutOfOrderSequence,
        ),
    ] {
        let budget = budget();
        let failed = collect(
            vec![
                fragment(0, true, &[b"first"]),
                fragment(sequence, false, &[payload]),
            ],
            limits(),
            &budget,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(failed.reason, expected);
        assert_eq!(
            failed.progress,
            ReportProgress {
                fragments: 1,
                items: 1,
                bytes: 5
            }
        );
        assert_eq!(failed.key, key());
        empty(&budget);
    }
    let budget = budget();
    let failed = collect(vec![fragment(1, false, &[])], limits(), &budget)
        .await
        .err()
        .unwrap();
    assert_eq!(failed.reason, ReportFailure::MissingOrOutOfOrderSequence);
    empty(&budget);
}

#[tokio::test]
async fn every_correlation_dimension_is_checked() {
    for field in 0..6 {
        let budget = budget();
        let mut wrong = fragment(0, false, &[b"foreign"]);
        match field {
            0 => wrong.key.station = StationId::new("other").unwrap(),
            1 => wrong.key.connection = CorrelationId::new("reconnected").unwrap(),
            2 => wrong.key.protocol = ProtocolEdition::Ocpp16j,
            3 => wrong.key.action = ProtocolActionName::new("NotifyMonitoringReport").unwrap(),
            4 => wrong.key.request_id += 1,
            _ => wrong.key.correlation = CorrelationId::new("other-command").unwrap(),
        }
        let failed = collect(vec![wrong], limits(), &budget).await.err().unwrap();
        assert_eq!(failed.reason, ReportFailure::CorrelationMismatch);
        assert_eq!(failed.progress, ReportProgress::default());
        empty(&budget);
    }
}

#[tokio::test]
async fn size_item_fragment_and_frame_limits_reject_without_retaining_partial_input() {
    for (fragments, expected) in [
        (
            vec![
                fragment(0, true, &[b"prefix"]),
                fragment(1, false, &[&[1; 32]]),
            ],
            ReportFailure::ByteLimit,
        ),
        (
            vec![
                fragment(0, true, &[b"prefix"]),
                fragment(1, false, &[b"", b"", b"", b""]),
            ],
            ReportFailure::ItemLimit,
        ),
        (
            vec![
                fragment(0, true, &[]),
                fragment(1, true, &[]),
                fragment(2, true, &[]),
            ],
            ReportFailure::FragmentLimit,
        ),
    ] {
        let budget = budget();
        let failed = collect(fragments, limits(), &budget).await.err().unwrap();
        assert_eq!(failed.reason, expected);
        empty(&budget);
    }
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits {
        maximum_ocpp_message_bytes: 4,
        ..RuntimeResourceLimits::default()
    })
    .unwrap();
    let failed = collect(vec![fragment(0, false, &[b"12345"])], limits(), &budget)
        .await
        .err()
        .unwrap();
    assert_eq!(failed.reason, ReportFailure::ByteLimit);
    empty(&budget);
}

#[tokio::test]
async fn exact_limits_and_empty_final_fragment_are_valid() {
    let budget = budget();
    let report = collect(vec![fragment(0, false, &[&[7; 32]])], limits(), &budget)
        .await
        .unwrap();
    assert_eq!(report.progress().bytes, 32);
    drop(report);
    let report = collect(
        vec![
            fragment(0, true, &[b"", b"", b"", b""]),
            fragment(1, true, &[]),
            fragment(2, false, &[]),
        ],
        limits(),
        &budget,
    )
    .await
    .unwrap();
    assert_eq!(
        report.progress(),
        ReportProgress {
            fragments: 3,
            items: 4,
            bytes: 0
        }
    );
    drop(report);
    empty(&budget);
}

#[tokio::test]
async fn invalid_limits_do_not_allocate_or_poll_source() {
    for bad in [
        ReportLimits {
            maximum_bytes: 0,
            ..limits()
        },
        ReportLimits {
            maximum_bytes: usize::MAX,
            ..limits()
        },
        ReportLimits {
            maximum_items: usize::MAX,
            ..limits()
        },
        ReportLimits {
            maximum_fragments: 0,
            ..limits()
        },
        ReportLimits {
            timeout: Duration::ZERO,
            ..limits()
        },
        ReportLimits {
            timeout: Duration::MAX,
            ..limits()
        },
    ] {
        let budget = budget();
        let failed = collect_report(
            key(),
            bad,
            budget.clone(),
            || {
                panic!("invalid collection must not poll ingress");
                #[allow(unreachable_code)]
                future::ready(Ok(None))
            },
            future::pending(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(failed.reason, ReportFailure::InvalidConfiguration);
        empty(&budget);
    }
}

#[tokio::test]
async fn shared_capacity_protects_charging_and_counts_completed_reports() {
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits {
        aggregate_queued_payload_bytes: 2048,
        reserved_critical_payload_bytes: 512,
        trace_ring_bytes: 0,
        queues: uob_application::RuntimeQueueLimits {
            multipart_assemblies: 1,
            ..Default::default()
        },
        ..RuntimeResourceLimits::default()
    })
    .unwrap();
    let report = collect(vec![fragment(0, false, &[b"one"])], limits(), &budget)
        .await
        .unwrap();
    let failed = collect(vec![], limits(), &budget).await.err().unwrap();
    assert!(matches!(failed.reason, ReportFailure::Capacity(_)));
    let charger = budget.try_reserve(WorkClass::ChargerRequest, 512).unwrap();
    drop(charger);
    drop(report);
    empty(&budget);
    let occupied = budget.try_reserve(WorkClass::DatabaseWork, 1536).unwrap();
    let failed = collect(vec![], limits(), &budget).await.err().unwrap();
    assert!(matches!(failed.reason, ReportFailure::Capacity(_)));
    drop(occupied);
    empty(&budget);
}
