use crate::support::time;
use serde_json::{Value, json};
use std::sync::Arc;
use uob_contracts::{LocalListUpdateType16, ResourceRef, SendLocalListReference16};
use uob_protocol_adapter::v16::remote_control::*;
pub fn reference() -> String {
    format!("list16:{}", "a".repeat(64))
}
pub fn native() -> Value {
    json!({"listVersion":-2,"updateType":"Full","localAuthorizationList":[{"idTag":"PRIVATE-MARKER","idTagInfo":{"status":"Accepted","parentIdTag":"PARENT-MARKER","expiryDate":"2099-01-01T00:00:00Z"}}]})
}
pub fn provider(resource: &ResourceRef) -> Arc<LocalAuthorizationUpdates16> {
    Arc::new(
        LocalAuthorizationUpdates16::new(vec![ProtectedLocalListValue16 {
            resource: resource.clone(),
            reference: reference(),
            update: ProtectedLocalListUpdate16::from_json_bytes(native().to_string().into_bytes())
                .unwrap(),
            expires_at: time("2026-09-02T00:00:00Z"),
        }])
        .unwrap(),
    )
}
#[test]
fn native_semantics_preserve_empty_unicode_deletion_and_reject_duplicate_or_non_native_input() {
    for update_type in ["Full", "Differential"] {
        for list in [None, Some(json!([]))] {
            let mut raw = json!({"listVersion":-2,"updateType":update_type});
            if let Some(list) = list {
                raw["localAuthorizationList"] = list;
            }
            assert!(
                ProtectedLocalListUpdate16::from_json_bytes(raw.to_string().into_bytes()).is_ok()
            );
        }
    }
    for status in ["Accepted", "Blocked", "Expired", "Invalid", "ConcurrentTx"] {
        let raw = json!({"listVersion":1,"updateType":"Full","localAuthorizationList":[{"idTag":"🦀".repeat(20),"idTagInfo":{"status":status,"parentIdTag":""}}]});
        assert!(ProtectedLocalListUpdate16::from_json_bytes(raw.to_string().into_bytes()).is_ok());
    }
    let deleted = json!({"listVersion":1,"updateType":"Differential","localAuthorizationList":[{"idTag":""}]});
    assert!(ProtectedLocalListUpdate16::from_json_bytes(deleted.to_string().into_bytes()).is_ok());
    let mut invalid = deleted;
    invalid["updateType"] = json!("Full");
    assert!(ProtectedLocalListUpdate16::from_json_bytes(invalid.to_string().into_bytes()).is_err());
    invalid["updateType"] = json!("Differential");
    invalid["localAuthorizationList"] = json!([{"idTag":"Straße"},{"idTag":"STRASSE"}]);
    assert!(ProtectedLocalListUpdate16::from_json_bytes(invalid.to_string().into_bytes()).is_err());
    invalid["localAuthorizationList"] = json!([{"idTag":"🦀".repeat(21)}]);
    assert!(ProtectedLocalListUpdate16::from_json_bytes(invalid.to_string().into_bytes()).is_err());
    invalid["localAuthorizationList"] =
        Value::Array((0..257).map(|n| json!({"idTag":n.to_string()})).collect());
    assert!(ProtectedLocalListUpdate16::from_json_bytes(invalid.to_string().into_bytes()).is_err());
    for version in [-1, 0] {
        assert!(
            ProtectedLocalListUpdate16::from_json_bytes(
                json!({"listVersion":version,"updateType":"Full"})
                    .to_string()
                    .into_bytes()
            )
            .is_err()
        );
    }
}
#[test]
fn immutable_capabilities_bind_exact_station_version_type_and_expiry_and_revocation() {
    let resource = ResourceRef {
        bridge_id: uob_contracts::BridgeId::new("bridge").unwrap(),
        station_id: uob_contracts::StationId::new("station").unwrap(),
        resource: None,
        native_protocol_reference: None,
    };
    let provider = provider(&resource);
    let mut request = SendLocalListReference16 {
        list_version: -2,
        update_type: LocalListUpdateType16::Full,
        update_reference: reference(),
    };
    assert!(provider.authorized(&resource, &request, time("2026-09-01T00:00:00Z")));
    request.list_version = 1;
    assert!(!provider.authorized(&resource, &request, time("2026-09-01T00:00:00Z")));
    request.list_version = -2;
    request.update_type = LocalListUpdateType16::Differential;
    assert!(!provider.authorized(&resource, &request, time("2026-09-01T00:00:00Z")));
    request.update_type = LocalListUpdateType16::Full;
    let mut foreign = resource.clone();
    foreign.station_id = uob_contracts::StationId::new("foreign").unwrap();
    assert!(!provider.authorized(&foreign, &request, time("2026-09-01T00:00:00Z")));
    assert!(!provider.authorized(&resource, &request, time("2026-09-02T00:00:00Z")));
    provider.revoke(&reference()).unwrap();
    assert!(!provider.authorized(&resource, &request, time("2026-09-01T00:00:00Z")));
}
