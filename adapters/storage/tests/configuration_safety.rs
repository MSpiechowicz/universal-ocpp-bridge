use serde_json::{Value, json};
use std::time::Duration;
use uob_application::{AtomicStoreWrite, OperationalStore, StorageErrorCode};
use uob_contracts::{Command, RequestId};
use uob_storage_adapter::SqliteOperationalStore;

#[tokio::test]
async fn sqlite_refuses_inline_secret_even_when_caller_bypasses_coordinator() {
    let root = std::env::temp_dir().join(format!("uob-config-guard-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let store =
        SqliteOperationalStore::<Value, Value, Value, Value>::open(root.join("state.db"), 8)
            .unwrap();
    let inline: Command<Value> = serde_json::from_value(json!({
        "schema_version":{"major":1,"revision":0}, "request_id":"inline-secret",
        "resource":{"bridge_id":"bridge-a","station_id":"station-a","native_protocol_reference":{"protocol":"ocpp16","connector_id":0}},
        "origin":{"kind":"management","principal_id":"operator-a"},
        "admitted_at":"2026-09-01T00:00:00Z", "expires_at":"2026-09-02T00:00:00Z",
        "operation":{"kind":"ocpp","parameters":{
            "protocol":"ocpp16j", "action":"ChangeConfiguration",
            "payload_schema":"urn:OCPP:1.6:2019:12:ChangeConfigurationRequest",
            "payload":{"key":"VendorPassword","value":"private-secret"}
        }}
    })).unwrap();
    let mut malicious_read = serde_json::to_value(&inline).unwrap();
    malicious_read["request_id"] = json!("read-inline-secret");
    malicious_read["operation"]["parameters"]["action"] = json!("GetConfiguration");
    malicious_read["operation"]["parameters"]["payload_schema"] =
        json!("urn:OCPP:1.6:2019:12:GetConfigurationRequest");
    let malicious_read: Command<Value> = serde_json::from_value(malicious_read).unwrap();
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(inline);
    let error = store.write_atomic(write).await.unwrap_err();
    assert_eq!(error.code(), StorageErrorCode::InvalidRequest);
    assert!(
        store
            .command_by_request_id(RequestId::new("inline-secret").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    let mut read_write = AtomicStoreWrite::empty();
    read_write.command = Some(malicious_read);
    assert_eq!(
        store.write_atomic(read_write).await.unwrap_err().code(),
        StorageErrorCode::InvalidRequest
    );
    assert!(
        store
            .command_by_request_id(RequestId::new("read-inline-secret").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let bytes = std::fs::read(root.join("state.db")).unwrap();
    assert!(
        !bytes
            .windows(b"private-secret".len())
            .any(|window| window == b"private-secret")
    );
    std::fs::remove_dir_all(root).unwrap();
}
