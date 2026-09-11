use super::*;
use uob_application::{FlowDiagnostics, TargetMessage, capture::*};
use uob_contracts::{CorrelationId, ProcessInstanceId, TraceRecord};
struct Clock;
impl uob_application::CommandClock for Clock {
    fn now(&self) -> uob_contracts::UtcTimestamp {
        timestamp()
    }
}
#[tokio::test]
async fn delivery_keeps_event_correlation_and_exact_destination_without_claiming_ack() {
    let mut fixture = fixture(vec![TargetBehavior::Run]);
    let capture = CaptureManager::new(true);
    let bridge = BridgeId::new("bridge-test").unwrap();
    let grant =
        CaptureGrant::new(bridge.clone(), vec![CapturePermission::Capture], None, None).unwrap();
    capture
        .start(
            &grant,
            CaptureFilter {
                bridge: bridge.clone(),
                station: None,
                target: None,
            },
            CaptureLevel::Metadata,
            None,
        )
        .unwrap();
    let (flow, rx) = FlowDiagnostics::channel(
        ProcessInstanceId::new("process").unwrap(),
        bridge,
        capture,
        Arc::new(Clock),
        8,
    )
    .unwrap();
    let (ingress, task) = uob_target_adapter::spawn_target_session_with_diagnostics(
        &fixture.selection,
        ports(
            Arc::new(RejectingCommands::default()),
            Arc::new(NoReports),
            Arc::new(SaturatedDiagnostics::default()),
        ),
        budget(),
        options(),
        flow,
    )
    .unwrap();
    let mut event = delivery("delivery-1", "target-main", 7);
    let TargetMessage::DomainEvent(payload) = Arc::get_mut(&mut event.message).unwrap() else {
        panic!("event")
    };
    payload.correlation_id = Some(CorrelationId::new("ocpp-origin").unwrap());
    ingress.try_deliver(event, 128).unwrap();
    assert_eq!(
        fixture.observations.recv().await.as_deref(),
        Some("delivery-1")
    );
    let records = rx
        .try_iter()
        .map(|r| serde_json::from_slice::<TraceRecord>(r.encoded_json()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(
        |r| r.correlation_id.as_ref().unwrap().as_str() == "ocpp-origin"
            && r.target.as_ref().unwrap().instance_id.as_str() == "target-main"
            && r.target.as_ref().unwrap().kind.as_str() == "test.session"
    ));
    assert_eq!(records[0].stage.as_str(), "target.mapping");
    assert_eq!(records[1].stage.as_str(), "target.enqueue");
    assert!(
        records
            .iter()
            .all(|r| r.redacted_details.as_ref().unwrap().fields["evidence"] == "completed")
    );
    task.shutdown(Duration::from_secs(1)).await.unwrap();
}
