use std::{sync::Arc, thread};

use super::*;

fn small_limits() -> RuntimeResourceLimits {
    RuntimeResourceLimits {
        maximum_connected_stations: 2,
        maximum_ocpp_message_bytes: 32,
        aggregate_queued_payload_bytes: 100,
        reserved_critical_payload_bytes: 20,
        trace_ring_bytes: 40,
        queues: RuntimeQueueLimits {
            database_work: 2,
            subscribers: 2,
            pending_requests: 2,
            multipart_assemblies: 2,
            target_ingress: 2,
            target_egress: 2,
            target_retries: 2,
            critical_reports: 2,
            diagnostics: 2,
            exporter_batches: 2,
            capture_records: 2,
        },
    }
}

#[test]
fn defaults_match_the_runtime_resource_policy() {
    let limits = RuntimeResourceLimits::default();

    assert_eq!(limits.maximum_connected_stations, 16);
    assert_eq!(limits.maximum_ocpp_message_bytes, 256 * 1024);
    assert_eq!(limits.aggregate_queued_payload_bytes, 16 * 1024 * 1024);
    assert_eq!(limits.trace_ring_bytes, 8 * 1024 * 1024);
    RuntimeResourceBudget::new(limits).expect("valid defaults");
}

#[test]
fn default_station_and_message_boundaries_reject_the_first_excess() {
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits::default()).expect("budget");
    let stations: Vec<_> = (0..DEFAULT_MAX_CONNECTED_STATIONS)
        .map(|_| budget.admit_station().expect("station within default"))
        .collect();

    assert_eq!(
        budget
            .admit_station()
            .expect_err("seventeenth station rejected")
            .limit,
        AdmissionLimit::ConnectedStations
    );
    budget
        .validate_ocpp_message(DEFAULT_MAX_OCPP_MESSAGE_BYTES)
        .expect("message at boundary");
    assert_eq!(
        budget
            .validate_ocpp_message(DEFAULT_MAX_OCPP_MESSAGE_BYTES + 1)
            .expect_err("first oversized message rejected")
            .limit,
        AdmissionLimit::OcppMessageBytes
    );

    drop(stations);
}

#[test]
fn station_and_protocol_message_limits_fail_predictably() {
    let budget = RuntimeResourceBudget::new(small_limits()).expect("budget");
    let first = budget.admit_station().expect("first station");
    let second = budget.admit_station().expect("second station");

    assert_eq!(
        budget.admit_station().expect_err("third station rejected"),
        AdmissionError {
            limit: AdmissionLimit::ConnectedStations,
            maximum: 2,
            requested: 3,
        }
    );
    assert_eq!(
        budget
            .validate_ocpp_message(33)
            .expect_err("oversized message rejected")
            .limit,
        AdmissionLimit::OcppMessageBytes
    );

    drop((first, second));
    assert_eq!(budget.snapshot().connected_stations, 0);
}

#[test]
fn independent_producers_share_one_aggregate_byte_cap() {
    let budget = Arc::new(RuntimeResourceBudget::new(small_limits()).expect("budget"));
    let first_budget = Arc::clone(&budget);
    let first = thread::spawn(move || {
        first_budget
            .try_reserve(WorkClass::DatabaseWork, 40)
            .expect("database reservation")
    });
    let second_budget = Arc::clone(&budget);
    let second = thread::spawn(move || {
        second_budget
            .try_reserve(WorkClass::TargetEgress, 40)
            .expect("target reservation")
    });
    let database = first.join().expect("database producer");
    let target = second.join().expect("target producer");

    assert_eq!(budget.snapshot().queued_payload_bytes, 80);
    assert_eq!(
        budget
            .try_reserve(WorkClass::Diagnostic, 1)
            .expect_err("shared noncritical cap reached")
            .limit,
        AdmissionLimit::AggregatePayloadBytes
    );

    drop((database, target));
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
}

#[test]
fn saturated_background_work_preserves_critical_progress() {
    let budget = RuntimeResourceBudget::new(small_limits()).expect("budget");
    let background = budget
        .try_reserve(WorkClass::TargetRetry, 80)
        .expect("background fills its allowance");

    assert!(budget.try_reserve(WorkClass::Diagnostic, 1).is_err());
    let charger = budget
        .try_reserve(WorkClass::ChargerRequest, 10)
        .expect("charger uses protected capacity");
    let completion = budget
        .try_reserve(WorkClass::CriticalReport, 10)
        .expect("critical completion uses protected capacity");
    assert_eq!(budget.snapshot().queued_payload_bytes, 100);

    drop((background, charger, completion));
}

#[test]
fn database_and_multipart_reservations_release_on_every_drop_path() {
    let budget = RuntimeResourceBudget::new(small_limits()).expect("budget");
    {
        let _database = budget
            .try_reserve(WorkClass::DatabaseWork, 30)
            .expect("database job");
        let _multipart = budget
            .try_reserve(WorkClass::MultipartAssembly, 40)
            .expect("multipart job");
        assert_eq!(budget.snapshot().queued_payload_bytes, 70);
    }
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
    assert!(budget.try_reserve(WorkClass::DatabaseWork, 80).is_ok());
}

#[test]
fn replaceable_telemetry_coalesces_without_consuming_another_item() {
    let budget = RuntimeResourceBudget::new(small_limits()).expect("budget");
    let mut slot = ReplaceableTelemetrySlot::new(WorkClass::TargetEgress);

    assert_eq!(
        slot.replace(&budget, "old", 30).expect("initial value"),
        TelemetryReplaceOutcome::Stored
    );
    assert_eq!(
        slot.replace(&budget, "new", 50).expect("replacement"),
        TelemetryReplaceOutcome::Replaced
    );
    assert_eq!(budget.snapshot().queued_payload_bytes, 50);
    assert_eq!(slot.take(), Some("new"));
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
}

#[test]
fn trace_hook_and_diagnostic_drop_metrics_are_bounded() {
    let budget = RuntimeResourceBudget::new(small_limits()).expect("budget");
    let trace = budget
        .try_reserve(WorkClass::CaptureTrace, 40)
        .expect("trace ring allocation");
    assert_eq!(
        budget
            .try_reserve(WorkClass::CaptureTrace, 1)
            .expect_err("trace ring full")
            .limit,
        AdmissionLimit::TraceRingBytes
    );
    budget.record_diagnostic_drop(DiagnosticDropReason::Full);
    budget.record_diagnostic_drop(DiagnosticDropReason::Closed);
    assert_eq!(budget.snapshot().dropped_diagnostics, 2);
    drop(trace);
}

#[test]
fn persistently_lagging_consumer_is_disconnected_after_bound() {
    let mut consumer = LaggingConsumer::new(2).expect("lag policy");

    assert_eq!(consumer.record_full(), LaggingConsumerAction::Retain);
    consumer.record_progress();
    assert_eq!(consumer.record_full(), LaggingConsumerAction::Retain);
    assert_eq!(consumer.record_full(), LaggingConsumerAction::Disconnect);
}
