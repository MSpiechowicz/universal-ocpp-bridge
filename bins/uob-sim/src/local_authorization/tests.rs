use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::{LocalAuthorizationConfig, LocalAuthorizationHandle, NativeUpdate};
use ocpp_client::ocpp_types::v16::common::SendLocalListResponseStatus as Status;
use serde_json::json;

#[test]
fn directory_sync_failure_after_replace_poisoned_until_explicit_disk_recovery() {
    let directory =
        std::env::temp_dir().join(format!("uob-commit-uncertain-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let config = LocalAuthorizationConfig {
        private_state_file: directory.join("state.json").to_str().unwrap().to_owned(),
        list_supported: true,
        cache_supported: true,
    };
    let model = LocalAuthorizationHandle::open("native-owner", &config).unwrap();
    let first: NativeUpdate = serde_json::from_value(
        json!({"listVersion":1,"updateType":"Full","localAuthorizationList":[{
            "idTag":"private-before","idTagInfo":{"status":"Accepted"}
        }]}),
    )
    .unwrap();
    assert_eq!(model.update(&first), Status::Accepted);
    model.0.lock().storage.as_mut().unwrap().fail_directory_sync = true;
    let next: NativeUpdate = serde_json::from_value(
        json!({"listVersion":2,"updateType":"Full","localAuthorizationList":[{
            "idTag":"private-after","idTagInfo":{"status":"Accepted"}
        }]}),
    )
    .unwrap();
    assert_eq!(model.update(&next), Status::Failed);
    assert_eq!(model.snapshot(), json!({"stateAvailable":false}));
    assert!(!model.authorize_offline("private-before", time::OffsetDateTime::now_utc()));
    assert!(!model.authorize_offline("private-after", time::OffsetDateTime::now_utc()));
    assert!(!model.clear_cache());
    assert_eq!(model.update(&first), Status::Failed);
    assert!(model.offline_start(json!({"connectorId":1,"idTag":"private-after","meterStart":0,"timestamp":"2026-10-04T00:00:00Z"})).is_err());
    model.0.lock().storage.as_mut().unwrap().fail_directory_sync = false;
    model.reload().unwrap();
    assert_eq!(model.snapshot()["listVersion"], 2);
    assert!(!model.authorize_offline("private-before", time::OffsetDateTime::now_utc()));
    assert!(model.authorize_offline("private-after", time::OffsetDateTime::now_utc()));
    drop(model);
    fs::remove_dir_all(directory).unwrap();
}
