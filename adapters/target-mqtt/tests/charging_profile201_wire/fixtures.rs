use serde_json::{Value, json};
use std::sync::Arc;
use uob_application::{TargetDelivery, TargetMessage};
use uob_contracts::{CommandLifecycle, CommandResult, ContractVersion};

use crate::support::fixtures::{TestEvent, result_delivery};

// Authoritative test-only retained results; no privileged target ingress is enabled.
pub fn delivery(id: &str, periods: usize) -> TargetDelivery<TestEvent> {
    let mut delivery = result_delivery("bridge-a", "station-a", id, id);
    let result = result_mut(&mut delivery);
    result.schema_version = ContractVersion::V1_CHARGING_PROFILE_201;
    result.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted: true,
        error: None,
    };
    result.charging_profile_201 = Some(serde_json::from_value(evidence(periods)).unwrap());
    delivery
}

pub fn clear_delivery(id: &str) -> TargetDelivery<TestEvent> {
    let mut delivery = delivery(id, 2);
    let result = result_mut(&mut delivery);
    result.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted: false,
        error: Some(uob_contracts::CommandError {
            code: uob_contracts::CommandErrorCode::ProtocolRejected,
            detail: None,
        }),
    };
    result.charging_profile_201 = Some(
        serde_json::from_value(json!({
            "action":"ClearChargingProfile", "request":{"charging_profile_criteria":{
                "charging_profile_purpose":"TxDefaultProfile"
            }}, "status":"Unknown", "reason_code":"NotFound"
        }))
        .unwrap(),
    );
    delivery
}

pub fn assert_clear_payload(bytes: &[u8]) {
    let decoded: CommandResult = serde_json::from_slice(bytes).unwrap();
    assert!(!decoded.charging_profile_201.unwrap().accepted());
    let payload: Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(
        payload["charging_profile_201"],
        json!({
            "action":"ClearChargingProfile", "request":{"charging_profile_criteria":{
                "charging_profile_purpose":"TxDefaultProfile"
            }}, "status":"Unknown", "reason_code":"NotFound"
        })
    );
    assert_eq!(payload["lifecycle"]["accepted"], false);
    assert!(payload.get("observed_effects").is_none());
}

pub fn evidence(periods: usize) -> Value {
    assert!((2..=1024).contains(&periods));
    let periods: Vec<_> = (0..periods)
        .map(|index| {
            if index == 0 {
                json!({"start_period":0,"limit":"900719925474099.1","number_phases":1,"phase_to_use":2})
            } else {
                json!({"start_period":index * 60,"limit":"0"})
            }
        })
        .collect();
    json!({"action":"SetChargingProfile","request":{"evse_id":2,"charging_profile":{
        "id":-118,"stack_level":2,"charging_profile_purpose":"TxProfile",
        "charging_profile_kind":"Relative","transaction_id":"native-TX-118",
        "charging_schedule":[{"id":-8,"charging_rate_unit":"W","min_charging_rate":"0",
            "charging_schedule_period":periods}]
    }},"status":"Accepted"})
}

pub fn result_mut(delivery: &mut TargetDelivery<TestEvent>) -> &mut CommandResult {
    let TargetMessage::CommandResult(result) = Arc::get_mut(&mut delivery.message).unwrap() else {
        unreachable!();
    };
    result
}

pub fn assert_payload(bytes: &[u8], count: usize) {
    let payload: Value = serde_json::from_slice(bytes).unwrap();
    jsonschema::draft202012::new(
        &serde_json::from_str::<Value>(include_str!(
            "../../../../crates/contracts/schemas/v1.7/command-result.schema.json"
        ))
        .unwrap(),
    )
    .unwrap()
    .validate(&payload)
    .unwrap();
    assert_eq!(payload["schema_version"], json!({"major":1,"revision":7}));
    assert_eq!(payload["charging_profile_201"], evidence(count));
    assert!(payload.get("charging_profile_16").is_none());
    assert!(payload.get("observed_effects").is_none());
    let decoded: CommandResult = serde_json::from_slice(bytes).unwrap();
    assert!(decoded.charging_profile_201.unwrap().accepted());
    // Native Accepted and broker delivery are acknowledgements, not physical charging proof.
}
