use super::*;
use std::sync::Arc;
use uob_contracts::{
    CommandLifecycle, ConfigurationResult, ConfigurationWriteStatus, ContractVersion,
};

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
    let online = peer.next_publish().await;
    peer.acknowledge(&online).await;

    assert_configuration_result(&mut target, &mut peer).await;

    assert_schedule_result(&mut target, &mut peer).await;

    assert_unsupported_results(&mut target, &mut peer).await;
    graceful_shutdown(&mut target, &mut peer, "uob/v1/demo/bridge-a/availability").await;
}

async fn assert_configuration_result(
    target: &mut RunningTarget,
    peer: &mut support::BrokerConnection,
) {
    let mut delivery = result_delivery("bridge-a", "station-a", "configuration", "write-1");
    let TargetMessage::CommandResult(result) =
        Arc::get_mut(&mut delivery.message).expect("unique result delivery")
    else {
        unreachable!();
    };
    result.schema_version = ContractVersion::V1_CONFIGURATION;
    result.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted: true,
        error: None,
    };
    result.configuration = Some(ConfigurationResult::Write {
        key: "HeartbeatInterval".to_owned(),
        status: ConfigurationWriteStatus::RebootRequired,
    });
    let expected = result.clone();

    target
        .host
        .try_deliver(delivery)
        .expect("configuration delivery");
    let publication = peer.next_publish().await;
    assert_publication(
        &publication,
        "uob/v1/demo/bridge-a/results/station-a/write-1",
        false,
    );
    let payload = json(&publication);
    assert_eq!(
        payload["schema_version"],
        serde_json::json!({"major": 1, "revision": 1})
    );
    assert_eq!(
        payload["configuration"],
        serde_json::json!({
            "kind": "write",
            "key": "HeartbeatInterval",
            "status": "RebootRequired"
        })
    );
    assert_eq!(
        serde_json::from_slice::<CommandResult>(&publication.payload)
            .expect("configuration result JSON"),
        expected
    );
    peer.acknowledge(&publication).await;
    let report = tokio::time::timeout(Duration::from_secs(2), target.host.next_report())
        .await
        .expect("configuration delivery report timeout")
        .expect("delivery report channel");
    assert!(matches!(
        report.outcome,
        DeliveryOutcome::Acknowledged { .. }
    ));
}

async fn assert_schedule_result(target: &mut RunningTarget, peer: &mut support::BrokerConnection) {
    let mut delivery = result_delivery("bridge-a", "station-a", "schedule", "schedule-1");
    let TargetMessage::CommandResult(result) =
        Arc::get_mut(&mut delivery.message).expect("unique schedule delivery")
    else {
        unreachable!();
    };
    schedule::attach(result);
    target
        .host
        .try_deliver(delivery)
        .expect("schedule delivery");
    let publication = peer.next_publish().await;
    assert_publication(
        &publication,
        "uob/v1/demo/bridge-a/results/station-a/schedule-1",
        false,
    );
    schedule::assert_payload(&publication.payload);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), target.host.next_report())
            .await
            .is_err(),
        "schedule delivery must remain unacknowledged until PUBACK"
    );
    peer.acknowledge(&publication).await;
    let report = tokio::time::timeout(Duration::from_secs(2), target.host.next_report())
        .await
        .expect("schedule delivery report timeout")
        .expect("delivery report channel");
    assert_eq!(report.delivery_id.as_str(), "schedule");
    match report.outcome {
        DeliveryOutcome::Acknowledged { scope, .. } => assert_eq!(scope.0, "mqtt.broker_received"),
        outcome => panic!("schedule PUBACK must report broker receipt, got {outcome:?}"),
    }
}

async fn assert_unsupported_results(
    target: &mut RunningTarget,
    peer: &mut support::BrokerConnection,
) {
    for revision in [2, u16::MAX] {
        let mut unsupported = result_delivery(
            "bridge-a",
            "station-a",
            &format!("unsupported-{revision}"),
            "unsupported",
        );
        let TargetMessage::CommandResult(result) =
            Arc::get_mut(&mut unsupported.message).expect("unique unsupported delivery")
        else {
            unreachable!();
        };
        result.schema_version = ContractVersion { major: 1, revision };
        target
            .host
            .try_deliver(unsupported)
            .expect("unsupported delivery");
        let report = tokio::time::timeout(Duration::from_secs(2), target.host.next_report())
            .await
            .expect("unsupported delivery report timeout")
            .expect("delivery report channel");
        assert_eq!(
            report.outcome,
            DeliveryOutcome::PermanentFailure {
                reason: "mqtt.canonical_identity_mismatch".to_owned(),
            }
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), peer.next_publish())
                .await
                .is_err(),
            "unsupported result revision must not publish"
        );
    }
}
