mod support;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::{
    BrokerConnection, TestBroker,
    fixtures::{TestEvent, result_delivery},
    harness::{RunningTarget, standard_capacities, start_target},
};
use uob_application::{DeliveryOutcome, DeliveryReport, TargetDelivery, TargetMessage};
use uob_contracts::{AuthenticatedCommandOrigin, CommandResult, ContractVersion, PrincipalId};
use uob_mqtt_target_adapter::MqttRuntimeOptions;

fn delivery(id: &str) -> TargetDelivery<TestEvent> {
    let mut delivery = result_delivery("bridge-a", "station-a", id, id);
    let result = result_mut(&mut delivery);
    result.schema_version = ContractVersion::V1_CHARGING_PROFILE_16;
    result.lifecycle = uob_contracts::CommandLifecycle::ProtocolResponse {
        accepted: true,
        error: None,
    };
    result.charging_profile_16=Some(serde_json::from_value(json!({"action":"SetChargingProfile","request":{"connector_id":1,"cs_charging_profiles":{
        "charging_profile_id":-117,"stack_level":2,"charging_profile_purpose":"TxDefaultProfile","charging_profile_kind":"Relative",
        "charging_schedule":{"charging_rate_unit":"W","charging_schedule_period":[{"start_period":0,"limit":"900719925474099.1","number_phases":4},{"start_period":60,"limit":"0"}]}
    }},"status":"Accepted"})).unwrap());
    delivery
}
fn result_mut(delivery: &mut TargetDelivery<TestEvent>) -> &mut CommandResult {
    let TargetMessage::CommandResult(result) = Arc::get_mut(&mut delivery.message).unwrap() else {
        unreachable!();
    };
    result
}
async fn report(target: &mut RunningTarget) -> DeliveryReport {
    tokio::time::timeout(Duration::from_secs(2), target.host.next_report())
        .await
        .unwrap()
        .unwrap()
}
async fn online(peer: &mut BrokerConnection) {
    let online = peer.next_publish().await;
    assert_eq!(online.topic, "uob/v1/demo/bridge-a/availability");
    peer.acknowledge(&online).await;
}
async fn quiet(peer: &mut BrokerConnection) {
    assert!(
        tokio::time::timeout(Duration::from_millis(100), peer.next_publish())
            .await
            .is_err()
    );
}
async fn shutdown(target: &mut RunningTarget, peer: &mut BrokerConnection) {
    target.host.request_shutdown();
    let offline = peer.next_publish().await;
    assert_eq!(offline.topic, "uob/v1/demo/bridge-a/availability");
    peer.acknowledge(&offline).await;
    peer.expect_disconnect().await;
    tokio::time::timeout(Duration::from_secs(2), &mut target.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn profile_result_publishes_exact_evidence_and_only_its_correlated_puback_marks_broker_receipt()
 {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    online(&mut peer).await;
    target.host.try_deliver(delivery("profile-first")).unwrap();
    let first = peer.next_publish().await;
    target.host.try_deliver(delivery("profile-second")).unwrap();
    let second = peer.next_publish().await;
    let payload: Value = serde_json::from_slice(&first.payload).unwrap();
    assert_eq!(payload["schema_version"], json!({"major":1,"revision":6}));
    assert_eq!(
        payload["charging_profile_16"]["request"]["cs_charging_profiles"]["charging_schedule"]["charging_schedule_period"]
            [0]["limit"],
        "900719925474099.1"
    );
    assert_eq!(
        payload["charging_profile_16"]["request"]["cs_charging_profiles"]["charging_schedule"]["charging_schedule_period"]
            [1]["limit"],
        "0"
    );
    assert_eq!(
        first.topic,
        "uob/v1/demo/bridge-a/results/station-a/profile-first"
    );
    assert_eq!(first.qos, 1);
    assert!(!first.retain);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), target.host.next_report())
            .await
            .is_err()
    );
    peer.acknowledge(&second).await;
    let complete = report(&mut target).await;
    assert_eq!(complete.delivery_id.as_str(), "profile-second");
    assert!(
        matches!(complete.outcome,DeliveryOutcome::Acknowledged {scope,..} if scope.0=="mqtt.broker_received")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), target.host.next_report())
            .await
            .is_err()
    );
    peer.acknowledge(&first).await;
    assert_eq!(
        report(&mut target).await.delivery_id.as_str(),
        "profile-first"
    );
    shutdown(&mut target, &mut peer).await;
}

#[tokio::test]
async fn future_versions_management_origin_and_oversized_profiles_fail_before_publication() {
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
    for case in ["future", "management", "large"] {
        let mut delivery = delivery(case);
        let result = result_mut(&mut delivery);
        match case {
            "future" => {
                result.schema_version = ContractVersion {
                    major: 1,
                    revision: ContractVersion::V1_RESERVATION_16.revision + 1,
                }
            }
            "management" => {
                result.return_route.origin = AuthenticatedCommandOrigin::Management {
                    principal_id: PrincipalId::new("operator").unwrap(),
                }
            }
            "large" => {
                let uob_contracts::ChargingProfileResult16::SetChargingProfile { request, .. } =
                    result.charging_profile_16.as_mut().unwrap()
                else {
                    unreachable!();
                };
                request
                    .cs_charging_profiles
                    .charging_schedule
                    .charging_schedule_period = (0..1024)
                    .map(|index| uob_contracts::CompositeSchedulePeriod16 {
                        start_period: index,
                        limit: uob_contracts::ExactDecimal::new(0, 0),
                        number_phases: Some(4),
                    })
                    .collect();
            }
            _ => unreachable!(),
        }
        target.host.try_deliver(delivery).unwrap();
        let outcome = report(&mut target).await.outcome;
        assert_eq!(
            outcome,
            DeliveryOutcome::PermanentFailure {
                reason: if case == "large" {
                    "mqtt.payload_too_large"
                } else {
                    "mqtt.canonical_identity_mismatch"
                }
                .to_owned()
            }
        );
        quiet(&mut peer).await;
    }
    target
        .host
        .try_deliver(delivery("valid-after-denials"))
        .unwrap();
    let publication = peer.next_publish().await;
    peer.acknowledge(&publication).await;
    assert_eq!(
        report(&mut target).await.delivery_id.as_str(),
        "valid-after-denials"
    );
    shutdown(&mut target, &mut peer).await;
}
