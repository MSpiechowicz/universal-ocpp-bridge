use super::*;
use support::fixtures::snapshot_delivery;
use uob_application::{TargetDiagnostic, TargetHealthState};
use uob_contracts::UtcTimestamp;

#[tokio::test]
async fn immediate_native_evidence_pubacks_never_acknowledge_the_pending_durable_delivery() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    online(&mut peer).await;
    ensure_subscription(&mut peer).await;
    target
        .host
        .try_deliver(snapshot_delivery("bridge-a", "station-a", "pending-state"))
        .unwrap();
    let pending = peer.next_publish().await;
    assert_eq!(pending.topic, "uob/v1/demo/bridge-a/state/station-a");
    for (index, (id, evidence)) in fixtures::cases().into_iter().enumerate() {
        submit(&mut peer, id, u16::try_from(40 + index).unwrap()).await;
        let submission = tokio::time::timeout(Duration::from_secs(2), target.host.next_command())
            .await
            .unwrap()
            .unwrap();
        let mut delivery = fixtures::delivery(id, evidence.clone());
        let result = fixtures::result_mut(&mut delivery);
        result.return_route.origin = submission.command.origin.clone();
        // Authoritative test-host evidence exercises result consumption, not native ingress.
        submission.respond(Ok(result.clone())).unwrap();
        let publication = peer.next_publish().await;
        assert_publication(&publication, id, evidence.as_ref());
        assert_ne!(publication.packet_id, pending.packet_id);
        peer.acknowledge(&publication).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), target.host.next_report())
                .await
                .is_err()
        );
    }
    peer.acknowledge(&pending).await;
    let completed = report(&mut target).await;
    assert_eq!(completed.delivery_id.as_str(), "pending-state");
    assert_broker_receipt(completed.outcome);
    shutdown(&mut target, &mut peer).await;
}

#[tokio::test]
async fn immediate_native_results_obey_version_and_complete_payload_limits_without_fallbacks() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions {
            maximum_message_bytes: 4096,
            ..MqttRuntimeOptions::default()
        },
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    online(&mut peer).await;
    ensure_subscription(&mut peer).await;
    let evidence = json!({"action":"SendLocalList","version_number":1,"update_type":"Full","status":"Accepted"});
    for (index, case) in ["revision-2", "revision-3", "future", "large", "valid"]
        .into_iter()
        .enumerate()
    {
        submit(&mut peer, case, u16::try_from(70 + index).unwrap()).await;
        let submission = tokio::time::timeout(Duration::from_secs(2), target.host.next_command())
            .await
            .unwrap()
            .unwrap();
        let mut delivery = fixtures::delivery(case, Some(evidence.clone()));
        let result = fixtures::result_mut(&mut delivery);
        result.return_route.origin = submission.command.origin.clone();
        alter(result, case);
        submission.respond(Ok(result.clone())).unwrap();
        if case == "valid" {
            let publication = peer.next_publish().await;
            assert!(publication.payload.len() <= 4096);
            assert_publication(&publication, case, Some(&evidence));
            peer.acknowledge(&publication).await;
        } else {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if matches!(target.host.next_diagnostic().await.unwrap(), TargetDiagnostic::Health(health)
                        if health.state == TargetHealthState::Degraded && health.reason.as_deref() == Some(rejection(case))) { break; }
                }
            }).await.unwrap();
            quiet(&mut peer).await;
        }
    }
    shutdown(&mut target, &mut peer).await;
}
async fn ensure_subscription(peer: &mut BrokerConnection) {
    if peer.subscriptions().is_empty() {
        peer.next_subscription().await;
    }
}
async fn submit(peer: &mut BrokerConnection, id: &str, packet: u16) {
    let request = json!({"schema_version":{"major":1,"revision":0},"request_id":id,
        "resource":{"bridge_id":"bridge-a","station_id":"station-a"},
        "operation":{"kind":"start","parameters":{}},
        "expires_at":UtcTimestamp::new(time::OffsetDateTime::now_utc() + Duration::from_secs(60))});
    peer.publish_command(
        &format!("uob/v1/demo/bridge-a/commands/station-a/{id}"),
        &serde_json::to_vec(&request).unwrap(),
        packet,
        false,
        false,
    )
    .await;
}
