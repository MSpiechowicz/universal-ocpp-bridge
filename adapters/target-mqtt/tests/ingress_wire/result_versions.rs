use super::*;
use uob_application::DeliveryOutcome;
use uob_contracts::{ConfigurationResult, ConfigurationWriteStatus};

#[tokio::test]
async fn publishes_configuration_and_composite_schedule_results_and_rejects_unsupported_revisions()
{
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    acknowledge_online(&mut peer).await;
    ensure_subscription(&mut peer, "uob/v1/demo/bridge-a/commands/+/+").await;

    for (packet_id, request_id, revision) in [
        (20, "configuration-write", 1),
        (21, "composite-schedule", 4),
        (22, "unsupported-2", 2),
        (24, "unsupported-5", 5),
    ] {
        let payload = command("bridge-a", "station-a", request_id, future());
        peer.publish_command(
            &format!("uob/v1/demo/bridge-a/commands/station-a/{request_id}"),
            &serde_json::to_vec(&payload).unwrap(),
            packet_id,
            false,
            false,
        )
        .await;
        let submission = tokio::time::timeout(Duration::from_secs(2), target.host.next_command())
            .await
            .expect("command admission timeout")
            .expect("command admission");
        let mut result = admitted(&submission.command);
        result.schema_version = ContractVersion { major: 1, revision };
        result.lifecycle = CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None,
        };
        let pending_delivery = if revision == 4 {
            schedule::attach(&mut result);
            target
                .host
                .try_deliver(snapshot_delivery("bridge-a", "station-a", "pending-state"))
                .expect("concurrent durable delivery");
            let publication = peer.next_publish().await;
            assert_eq!(publication.topic, "uob/v1/demo/bridge-a/state/station-a");
            Some(publication)
        } else {
            result.configuration = Some(ConfigurationResult::Write {
                key: "HeartbeatInterval".to_owned(),
                status: ConfigurationWriteStatus::RebootRequired,
            });
            None
        };
        submission
            .respond(Ok(result.clone()))
            .expect("host response");

        if revision == 1 || revision == 4 {
            assert_supported_result(
                &mut target,
                &mut peer,
                request_id,
                revision,
                &result,
                pending_delivery,
            )
            .await;
        } else {
            assert_unsupported_result(&mut target, &mut peer).await;
        }
    }

    shutdown(&mut target, &mut peer).await;
}

async fn assert_supported_result(
    target: &mut support::harness::RunningTarget,
    peer: &mut support::BrokerConnection,
    request_id: &str,
    revision: u16,
    result: &CommandResult,
    pending_delivery: Option<Publication>,
) {
    let published = peer.next_publish().await;
    assert_eq!(
        published.topic,
        format!("uob/v1/demo/bridge-a/results/station-a/{request_id}")
    );
    assert_eq!(published.qos, 1);
    assert!(!published.retain);
    if revision == 4 {
        schedule::assert_payload(&published.payload);
    } else {
        let payload: Value =
            serde_json::from_slice(&published.payload).expect("configuration result JSON");
        assert_eq!(
            payload["schema_version"],
            json!({"major": 1, "revision": 1})
        );
        assert_eq!(
            payload["configuration"],
            json!({
                "kind": "write",
                "key": "HeartbeatInterval",
                "status": "RebootRequired"
            })
        );
        assert_eq!(
            serde_json::from_slice::<CommandResult>(&published.payload)
                .expect("canonical configuration result"),
            *result
        );
    }
    peer.acknowledge(&published).await;
    if let Some(pending) = pending_delivery {
        assert!(
            tokio::time::timeout(Duration::from_millis(100), target.host.next_report())
                .await
                .is_err(),
            "immediate schedule PUBACK must not acknowledge the pending durable delivery"
        );
        peer.acknowledge(&pending).await;
        let report = tokio::time::timeout(Duration::from_secs(2), target.host.next_report())
            .await
            .expect("pending durable delivery report timeout")
            .expect("delivery report channel");
        assert_eq!(report.delivery_id.as_str(), "pending-state");
        assert!(matches!(
            report.outcome,
            DeliveryOutcome::Acknowledged { .. }
        ));
    }
}

async fn assert_unsupported_result(
    target: &mut support::harness::RunningTarget,
    peer: &mut support::BrokerConnection,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let diagnostic = target
                .host
                .next_diagnostic()
                .await
                .expect("diagnostic channel");
            if matches!(
                diagnostic,
                TargetDiagnostic::Health(health)
                    if health.state == TargetHealthState::Degraded
                        && health.reason.as_deref()
                            == Some("mqtt.canonical_identity_mismatch")
            ) {
                break;
            }
        }
    })
    .await
    .expect("unsupported version diagnostic timeout");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), peer.next_publish())
            .await
            .is_err(),
        "unsupported result revision must not publish"
    );
}
