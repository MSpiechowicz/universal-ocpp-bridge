use crate::configuration_support::reference;
use crate::{configuration_support::*, support::*};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uob_application::CommandAdmissionPort;
use uob_contracts::*;
use uob_protocol_adapter::v201::remote_control::configuration201_values::ProtectedNetworkProfile201;

#[tokio::test]
async fn complete_native_profile_slot_zero_preserves_fields_and_exact_staged_statuses() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let fixture: Value = serde_json::from_slice(include_bytes!(
        "../../../../tests/ocpp-fixtures/corpus/wire/2.0.1/set-network-profile-apn-vpn.json"
    ))
    .unwrap();
    let profile = fixture[3]["connectionData"].clone();
    let (snapshot, _, _, commands) = Box::pin(configured(
        &store,
        &running,
        vec![],
        Some(&profile),
        Arc::new(Clock),
    ))
    .await;
    for (status, expected) in [
        ("Accepted", SetNetworkProfileStatus201::Accepted),
        ("Rejected", SetNetworkProfileStatus201::Rejected),
        ("Failed", SetNetworkProfileStatus201::Failed),
    ] {
        let id = format!("network-{status}");
        let request = external(
            &snapshot,
            &id,
            "SetNetworkProfile",
            json!({"configurationSlot":0,"profileReference":reference(999)}),
        );
        let submit = {
            let commands = commands.clone();
            tokio::spawn(async move { commands.submit(request).await.unwrap() })
        };
        assert_eq!(
            receive_json(&mut running.peer).await,
            json!([2,id,"SetNetworkProfile",{"configurationSlot":0,"connectionData":profile}])
        );
        running.peer.send_text(json!([3,id,{"status":status,"statusInfo":{"reasonCode":"SYNTHETIC_ONLY","additionalInfo":"SYNTHETIC_VPN_PASS"}}]).to_string()).await.unwrap();
        let result = submit.await.unwrap();
        assert_eq!(
            result.configuration_201,
            Some(ConfigurationResult201::SetNetworkProfile {
                configuration_slot: 0,
                status: expected,
                staged: expected == SetNetworkProfileStatus201::Accepted,
            })
        );
        assert!(result.observed_effects.is_empty());
        assert!(
            matches!(result.lifecycle, CommandLifecycle::ProtocolResponse { accepted, .. } if accepted == (expected == SetNetworkProfileStatus201::Accepted))
        );
        let serialized = serde_json::to_string(&result).unwrap();
        for forbidden in [
            "SYNTHETIC_VPN_PASS",
            "connectionData",
            "profileReference",
            "statusInfo",
            "cfg201:",
        ] {
            assert!(!serialized.contains(forbidden));
        }
    }
    assert_no_secrets(
        &database,
        &[
            "SYNTHETIC_APN_PASS",
            "SYNTHETIC_VPN_PASS",
            "SYNTHETIC_ONLY_VPN_SHARED_KEY",
        ],
    );
    stop(running, store).await;
}

#[test]
fn profiles_reject_unknown_fields_bad_enums_and_out_of_native_integer_range() {
    let fixture: Value = serde_json::from_slice(include_bytes!(
        "../../../../tests/ocpp-fixtures/corpus/wire/2.0.1/set-network-profile-apn-vpn.json"
    ))
    .unwrap();
    let profile = fixture[3]["connectionData"].clone();
    assert!(
        ProtectedNetworkProfile201::from_json_bytes(serde_json::to_vec(&profile).unwrap()).is_ok()
    );
    for (field, bad) in [
        ("ocppVersion", json!("OCPP201")),
        ("messageTimeout", json!(2_147_483_648_i64)),
        ("unknownSecret", json!("REJECTED_SECRET")),
    ] {
        let mut invalid = profile.clone();
        invalid[field] = bad;
        assert!(
            ProtectedNetworkProfile201::from_json_bytes(serde_json::to_vec(&invalid).unwrap())
                .is_err()
        );
    }
}
