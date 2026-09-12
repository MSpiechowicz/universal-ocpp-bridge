use super::flow::*;
use crate::{capture::*, *};
use std::sync::Arc;
use uob_contracts::*;
struct Clock;
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH)
    }
}
fn setup(
    capacity: usize,
) -> (
    FlowDiagnostics,
    std::sync::mpsc::Receiver<SanitizedDiagnostic>,
    CaptureManager,
    CaptureGrant,
    CaptureFilter,
) {
    let manager = CaptureManager::new(true);
    let bridge = BridgeId::new("bridge").unwrap();
    let filter = CaptureFilter {
        bridge: bridge.clone(),
        station: None,
        target: None,
    };
    let grant = CaptureGrant::new(
        bridge.clone(),
        vec![CapturePermission::Capture, CapturePermission::Read],
        None,
        None,
    )
    .unwrap();
    let (flow, rx) = FlowDiagnostics::channel(
        ProcessInstanceId::new("process").unwrap(),
        bridge,
        manager.clone(),
        Arc::new(Clock),
        capacity,
    )
    .unwrap();
    (flow, rx, manager, grant, filter)
}
#[test]
fn disabled_filtered_stopped_and_full_capture_never_block_processing() {
    let (flow, rx, manager, grant, mut filter) = setup(1);
    let span = flow.span(None, Some(StationId::new("a").unwrap()), None);
    span.emit(FlowStage::CommandIngress, FlowEvidence::Completed);
    assert!(rx.try_recv().is_err());
    filter.station = Some(StationId::new("a").unwrap());
    let status = manager
        .start(&grant, filter, CaptureLevel::Metadata, None)
        .unwrap();
    flow.span(None, Some(StationId::new("b").unwrap()), None)
        .emit(FlowStage::Application, FlowEvidence::Completed);
    assert!(rx.try_recv().is_err());
    span.emit(FlowStage::CommandIngress, FlowEvidence::Completed);
    for _ in 0..1000 {
        span.emit(FlowStage::CommandDispatch, FlowEvidence::Uncertain);
    }
    assert_eq!(flow.dropped(), 1000);
    let trace: TraceRecord = serde_json::from_slice(rx.try_recv().unwrap().encoded_json()).unwrap();
    assert!(trace.correlation_id.is_none());
    assert_eq!(
        trace.redacted_details.unwrap().fields["correlation"],
        "uncorrelated"
    );
    manager.stop(&grant, status.id).unwrap();
    span.emit(FlowStage::ObservedEffect, FlowEvidence::Observed);
    assert!(rx.try_recv().is_err());
}
#[test]
fn async_context_preserves_identity_and_device_clock_cannot_claim_latency() {
    let (flow, rx, manager, grant, filter) = setup(8);
    manager
        .start(&grant, filter, CaptureLevel::Metadata, None)
        .unwrap();
    let span = flow.span(
        Some(CorrelationId::new("request").unwrap()),
        None,
        Some(ProtocolEdition::Ocpp201),
    );
    let worker = span.clone();
    std::thread::spawn(move || {
        let device =
            UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(10000));
        worker.source_time(FlowStage::OcppReceive, device);
        worker.emit(FlowStage::ProtocolResponse, FlowEvidence::Uncertain);
    })
    .join()
    .unwrap();
    let records = rx
        .try_iter()
        .map(|r| serde_json::from_slice::<TraceRecord>(r.encoded_json()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|r| r.correlation_id.as_ref().unwrap().as_str() == "request"
                && r.observed_at == Clock.now()
                && r.duration_micros.unwrap() < 60_000_000)
    );
    assert!(matches!(records[1].outcome, TraceOutcome::Uncertain { .. }));
    assert!(
        records[0]
            .redacted_details
            .as_ref()
            .unwrap()
            .fields
            .contains_key("source_time")
    );
    assert!(records[0].trace_sequence < records[1].trace_sequence);
}
#[test]
fn oversized_metadata_is_shed_and_state_projection_never_copies_raw_values() {
    let (flow, rx, manager, grant, filter) = setup(8);
    manager
        .start(&grant, filter, CaptureLevel::Metadata, None)
        .unwrap();
    flow.span(
        Some(CorrelationId::new("x".repeat(65536)).unwrap()),
        None,
        None,
    )
    .emit(FlowStage::Application, FlowEvidence::Completed);
    assert_eq!(flow.dropped(), 1);
    assert!(rx.try_recv().is_err());
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .unwrap();
    let before = DiagnosticState::capture(&snapshot);
    snapshot.resources[0].availability = AvailabilityState::Faulted;
    before.emit_changes(&snapshot, &flow.span(None, None, None));
    let record = rx.try_recv().unwrap();
    let text = std::str::from_utf8(record.encoded_json()).unwrap();
    assert!(text.contains("availability"));
    assert!(!text.contains("transactions"));
    assert!(!text.contains("current_values"));
    assert!(record.encoded_json().len() < 2048);
}

#[test]
fn retained_emitter_reports_process_lifetime_formatting_and_admission_drops() {
    let manager = CaptureManager::with_ring_limits(true, 1, 1).unwrap();
    let bridge = BridgeId::new("bridge").unwrap();
    let filter = CaptureFilter {
        bridge: bridge.clone(),
        station: None,
        target: None,
    };
    let grant = CaptureGrant::new(
        bridge.clone(),
        vec![CapturePermission::Capture, CapturePermission::Read],
        None,
        None,
    )
    .unwrap();
    let flow = FlowDiagnostics::retained(
        ProcessInstanceId::new("process").unwrap(),
        bridge,
        manager.clone(),
        Arc::new(Clock),
    );
    flow.span(None, None, None)
        .emit(FlowStage::Application, FlowEvidence::Completed);
    assert_eq!(flow.dropped(), 0);
    let capture = manager
        .start(&grant, filter.clone(), CaptureLevel::Metadata, None)
        .unwrap();
    flow.span(
        Some(CorrelationId::new("x".repeat(2048)).unwrap()),
        None,
        None,
    )
    .emit(FlowStage::Application, FlowEvidence::Completed);
    assert_eq!(flow.dropped(), 1);
    flow.span(None, None, None)
        .emit(FlowStage::Application, FlowEvidence::Completed);
    assert_eq!(flow.dropped(), 2);
    let lease = manager.lease(&grant, capture.id, false).unwrap();
    assert_eq!(lease.read_after(None).unwrap().window.dropped_records, 2);
    manager.stop(&grant, capture.id).unwrap();
    assert_eq!(flow.dropped(), 2);
    let next = manager
        .start(&grant, filter, CaptureLevel::Metadata, None)
        .unwrap();
    assert_eq!(flow.dropped(), 2);
    assert_eq!(
        manager
            .lease(&grant, next.id, false)
            .unwrap()
            .read_after(None)
            .unwrap()
            .window
            .dropped_records,
        0
    );
}

#[test]
fn command_metadata_is_typed_bounded_and_survives_cloned_spans() {
    let (flow, rx, manager, grant, filter) = setup(8);
    manager
        .start(&grant, filter, CaptureLevel::Metadata, None)
        .unwrap();
    let span = flow
        .span(None, None, None)
        .with_request(RequestId::new("request-1").unwrap());
    span.clone().emit_fields(
        FlowStage::Application,
        FlowEvidence::NotTransmitted,
        vec![
            SafeDiagnosticField::CommandReason(CommandErrorCode::StationDisconnected),
            SafeDiagnosticField::AccessReason(AccessPolicyError::ResourceDenied),
            SafeDiagnosticField::ObservedEvent(EventId::new("event-1").unwrap()),
        ],
    );
    let record: TraceRecord =
        serde_json::from_slice(rx.try_recv().unwrap().encoded_json()).unwrap();
    let fields = record.redacted_details.unwrap().fields;
    assert_eq!(fields["command.request_id"], "request-1");
    assert_eq!(fields["reason_code"], "ResourceDenied");
    assert_eq!(fields["command.event_id"], "event-1");
    flow.span(None, None, None)
        .with_request(RequestId::new("r".repeat(257)).unwrap())
        .emit(FlowStage::CommandIngress, FlowEvidence::Completed);
    span.emit_fields(
        FlowStage::CommandIngress,
        FlowEvidence::Completed,
        vec![SafeDiagnosticField::CommandOrigin(
            AuthenticatedCommandOrigin::Management {
                principal_id: PrincipalId::new("p".repeat(257)).unwrap(),
            },
        )],
    );
    assert!(rx.try_recv().is_err());
    assert_eq!(flow.dropped(), 2);
}
