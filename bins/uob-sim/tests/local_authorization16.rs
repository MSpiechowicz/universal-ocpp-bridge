use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use ocpp_client::ocpp_types::v16::common::SendLocalListResponseStatus as Status;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uob_sim::local_authorization::{
    LocalAuthorizationConfig, LocalAuthorizationHandle, NativeInfo, NativeUpdate,
};

struct PrivateDirectory(PathBuf);
impl PrivateDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("uob-list-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    fn config(&self) -> LocalAuthorizationConfig {
        LocalAuthorizationConfig {
            private_state_file: self.0.join("state.json").to_str().unwrap().to_owned(),
            list_supported: true,
            cache_supported: true,
        }
    }
}
impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn update(value: Value) -> NativeUpdate {
    serde_json::from_value(value).unwrap()
}
fn info(value: Value) -> NativeInfo {
    serde_json::from_value(value).unwrap()
}
fn full(version: i32, token: &str, status: &str) -> NativeUpdate {
    update(
        json!({"listVersion":version,"updateType":"Full", "localAuthorizationList":[{"idTag":token,"idTagInfo":{"status":status}}]}),
    )
}

#[test]
fn native_full_differential_empty_signed_versions_and_deletion() {
    let model = LocalAuthorizationHandle::in_memory("native-station");
    assert_eq!(
        model.update(&full(-2, "Private-Token", "Accepted")),
        Status::Accepted
    );
    assert_eq!(model.snapshot()["listVersion"], -2);
    assert!(model.authorize_offline("private-token", OffsetDateTime::now_utc()));
    for version in [-1, 0] {
        assert_eq!(
            model.update(&full(version, "new", "Accepted")),
            Status::Failed
        );
    }
    assert_eq!(
        model.update(&update(
            json!({"listVersion":-2,"updateType":"Differential"})
        )),
        Status::VersionMismatch
    );
    assert_eq!(
        model.update(&update(
            json!({"listVersion":3,"updateType":"Differential"})
        )),
        Status::Accepted
    );
    assert_eq!(model.snapshot()["listEntries"], 1);
    assert_eq!(
        model.update(&update(
            json!({"listVersion":4,"updateType":"Differential","localAuthorizationList":[]})
        )),
        Status::Accepted
    );
    assert_eq!(model.snapshot()["listEntries"], 1);
    assert_eq!(model.update(&update(json!({"listVersion":5,"updateType":"Differential","localAuthorizationList":[{"idTag":"PRIVATE-TOKEN"}]}))), Status::Accepted);
    assert_eq!(model.snapshot()["listVersion"], 0);
    assert!(!model.authorize_offline("private-token", OffsetDateTime::now_utc()));
    assert_eq!(model.update(&update(json!({"listVersion":6,"updateType":"Full","localAuthorizationList":[{"idTag":"missing-info"}]}))), Status::Failed);
    for entries in [None, Some(json!([]))] {
        assert_eq!(
            model.update(&full(i32::MAX, "present", "Accepted")),
            Status::Accepted
        );
        let mut value = json!({"listVersion":i32::MIN,"updateType":"Full"});
        if let Some(entries) = entries {
            value["localAuthorizationList"] = entries;
        }
        assert_eq!(model.update(&update(value)), Status::Accepted);
        assert_eq!(model.snapshot()["listEntries"], 0);
    }
    assert!(
        serde_json::from_value::<NativeUpdate>(
            json!({"listVersion":2_147_483_648_i64,"updateType":"Full"})
        )
        .is_err()
    );
}

#[test]
fn native_casefold_length_status_expiry_and_cache_priority() {
    let model = LocalAuthorizationHandle::in_memory("native-station");
    let duplicated = update(
        json!({"listVersion":1,"updateType":"Full","localAuthorizationList":[
            {"idTag":"Straße","idTagInfo":{"status":"Accepted"}}, {"idTag":"STRASSE","idTagInfo":{"status":"Accepted"}}
        ]}),
    );
    assert_eq!(model.update(&duplicated), Status::Failed);
    assert_eq!(
        model.update(&full(1, &"界".repeat(20), "Accepted")),
        Status::Accepted
    );
    assert_eq!(
        model.update(&full(2, &"界".repeat(21), "Accepted")),
        Status::Failed
    );
    assert_eq!(model.update(&full(2, "", "Accepted")), Status::Accepted);
    for status in ["Blocked", "Expired", "Invalid", "ConcurrentTx"] {
        assert_eq!(
            model.update(&full(3, "private-list", status)),
            Status::Accepted
        );
        assert!(!model.authorize_offline("PRIVATE-LIST", OffsetDateTime::now_utc()));
    }
    assert_eq!(model.update(&update(json!({"listVersion":4,"updateType":"Full","localAuthorizationList":[{
        "idTag":"private-list","idTagInfo":{"status":"Accepted","expiryDate":"2000-01-01T00:00:00Z","parentIdTag":"private-parent"}
    }]}))), Status::Accepted);
    assert!(!model.authorize_offline("private-list", OffsetDateTime::now_utc()));
    assert!(
        model
            .observe_central("PRIVATE-LIST", info(json!({"status":"Accepted"})))
            .unwrap()
    );
    assert_eq!(model.snapshot()["cacheEntries"], 0);
    model
        .observe_central(
            "private-cache",
            info(json!({"status":"Accepted","parentIdTag":"private-parent"})),
        )
        .unwrap();
    assert!(model.authorize_offline("PRIVATE-CACHE", OffsetDateTime::now_utc()));
    assert_eq!(model.snapshot()["cacheEntries"], 1);
    assert!(model.clear_cache());
    assert_eq!(model.snapshot()["listEntries"], 1);
    assert!(!model.authorize_offline("private-cache", OffsetDateTime::now_utc()));
    assert!(!model.snapshot().to_string().contains("private-"));
}

#[test]
fn atomic_private_persistence_ownership_recovery_and_failed_updates() {
    let directory = PrivateDirectory::new();
    let config = directory.config();
    let model = LocalAuthorizationHandle::open("station-owner", &config).unwrap();
    assert!(LocalAuthorizationHandle::open("station-owner", &config).is_err());
    assert_eq!(
        model.update(&full(1, "private-persisted", "Accepted")),
        Status::Accepted
    );
    model
        .observe_central("private-cached", info(json!({"status":"Accepted"})))
        .unwrap();
    let path = PathBuf::from(&config.private_state_file);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    assert_eq!(
        model.update(&full(2, "replacement", "Accepted")),
        Status::Failed
    );
    assert_eq!(model.snapshot()["listVersion"], 1);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    drop(model);
    assert!(LocalAuthorizationHandle::open("wrong-owner", &config).is_err());
    let recovered = LocalAuthorizationHandle::open("station-owner", &config).unwrap();
    assert!(recovered.authorize_offline("PRIVATE-PERSISTED", OffsetDateTime::now_utc()));
    assert_eq!(recovered.snapshot()["cacheEntries"], 1);
    drop(recovered);
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["format_version"] = json!(2);
    fs::write(&path, state.to_string()).unwrap();
    assert!(matches!(
        LocalAuthorizationHandle::open("station-owner", &config),
        Err("private_state_version")
    ));
    fs::write(&path, b"not JSON").unwrap();
    assert!(matches!(
        LocalAuthorizationHandle::open("station-owner", &config),
        Err("private_state_corrupt")
    ));
}

#[test]
fn unsupported_list_cache_and_capacity_are_native_not_positive_fallbacks() {
    let directory = PrivateDirectory::new();
    let mut config = directory.config();
    config.list_supported = false;
    config.cache_supported = false;
    let model = LocalAuthorizationHandle::open("station", &config).unwrap();
    assert_eq!(model.snapshot()["listVersion"], -1);
    assert_eq!(
        model.update(&full(1, "private", "Accepted")),
        Status::NotSupported
    );
    assert!(!model.clear_cache());
    let model = LocalAuthorizationHandle::in_memory("bounded");
    let entries: Vec<_> = (0..256)
        .map(|id| json!({"idTag":format!("private-{id}"),"idTagInfo":{"status":"Accepted"}}))
        .collect();
    assert_eq!(
        model.update(&update(
            json!({"listVersion":1,"updateType":"Full","localAuthorizationList":entries})
        )),
        Status::Accepted
    );
    assert_eq!(model.update(&update(json!({"listVersion":2,"updateType":"Differential","localAuthorizationList":[{"idTag":"overflow","idTagInfo":{"status":"Accepted"}}]}))), Status::Failed);
    assert_eq!(model.snapshot()["listVersion"], 1);
    for id in 0..256 {
        model
            .observe_central(&format!("cache-{id}"), info(json!({"status":"Accepted"})))
            .unwrap();
    }
    assert!(
        model
            .observe_central("cache-overflow", info(json!({"status":"Accepted"})))
            .is_err()
    );
    assert_eq!(model.snapshot()["cacheEntries"], 256);
}

#[test]
fn native_schema_preserves_absence_but_rejects_explicit_null_and_unknown_fields() {
    for request in [
        json!({"listVersion":1,"updateType":"Full","localAuthorizationList":null}),
        json!({"listVersion":1,"updateType":"Differential","localAuthorizationList":[{"idTag":"x","idTagInfo":null}]}),
        json!({"listVersion":1,"updateType":"Full","localAuthorizationList":[{"idTag":"x","idTagInfo":{"status":"Accepted","expiryDate":null}}]}),
        json!({"listVersion":1,"updateType":"Full","localAuthorizationList":[{"idTag":"x","idTagInfo":{"status":"Accepted","parentIdTag":null}}]}),
        json!({"listVersion":1,"updateType":"Full","extra":true}),
    ] {
        assert!(serde_json::from_value::<NativeUpdate>(request).is_err());
    }
}

#[test]
fn unicode_native_update_byte_budget_fails_without_replacing_prior_contents() {
    let entries: Vec<Value> = (0..256)
        .map(|id| {
            json!({
                "idTag":format!("{}{:03}", "💳".repeat(17), id),
                "idTagInfo":{"status":"Accepted","parentIdTag":"💳".repeat(20),
                    "expiryDate":"2099-01-01T00:00:00.000000000+00:00"}
            })
        })
        .collect();
    let model = LocalAuthorizationHandle::in_memory("native-byte-budget");
    let smaller = update(
        json!({"listVersion":1,"updateType":"Full","localAuthorizationList":&entries[..200]}),
    );
    assert!(serde_json::to_vec(&smaller).unwrap().len() < 64 * 1024);
    assert_eq!(model.update(&smaller), Status::Accepted);
    let oversized =
        update(json!({"listVersion":2,"updateType":"Full","localAuthorizationList":entries}));
    assert!(serde_json::to_vec(&oversized).unwrap().len() > 64 * 1024);
    assert_eq!(model.update(&oversized), Status::Failed);
    assert_eq!(model.snapshot()["listVersion"], 1);
    assert_eq!(model.snapshot()["listEntries"], 200);
}

#[test]
fn private_file_alias_binding_and_byte_limits_never_recover_as_empty() {
    use std::os::unix::fs::symlink;
    let directory = PrivateDirectory::new();
    let config = directory.config();
    drop(LocalAuthorizationHandle::open("bound-owner", &config).unwrap());
    let path = PathBuf::from(&config.private_state_file);
    let original = fs::read(&path).unwrap();
    let alias = directory.0.join("alias.json");
    fs::hard_link(&path, &alias).unwrap();
    assert!(LocalAuthorizationHandle::open("bound-owner", &config).is_err());
    fs::remove_file(&alias).unwrap();
    fs::rename(&path, &alias).unwrap();
    symlink(&alias, &path).unwrap();
    assert!(LocalAuthorizationHandle::open("bound-owner", &config).is_err());
    fs::remove_file(&path).unwrap();
    fs::rename(&alias, &path).unwrap();
    for (field, value) in [
        ("protocol", json!("ocpp2.0.1")),
        ("version", json!(-1)),
        ("next_offline_id", json!(0)),
    ] {
        let mut invalid: Value = serde_json::from_slice(&original).unwrap();
        invalid[field] = value;
        fs::write(&path, invalid.to_string()).unwrap();
        assert!(LocalAuthorizationHandle::open("bound-owner", &config).is_err());
    }
    fs::write(&path, vec![b' '; 2 * 1024 * 1024 + 1]).unwrap();
    assert!(LocalAuthorizationHandle::open("bound-owner", &config).is_err());
    fs::write(&path, original).unwrap();
    assert_eq!(
        LocalAuthorizationHandle::open("bound-owner", &config)
            .unwrap()
            .snapshot()["listVersion"],
        0
    );
}

#[test]
fn offline_capacity_is_bounded_and_native_wide_meter_readings_are_preserved() {
    let directory = PrivateDirectory::new();
    let config = directory.config();
    let model = LocalAuthorizationHandle::open("queue-owner", &config).unwrap();
    assert_eq!(
        model.update(&full(1, "private-queue", "Accepted")),
        Status::Accepted
    );
    for _ in 0..128 {
        assert!(
            model
                .offline_start(json!({"connectorId":1,"idTag":"PRIVATE-QUEUE",
            "meterStart":i64::MAX - 1,"timestamp":"2026-09-01T00:00:00Z"}))
                .unwrap()
        );
        model
            .offline_stop(
                1,
                json!({"meterStop":i64::MAX,
            "timestamp":"2026-08-31T23:59:59Z"}),
            )
            .unwrap();
    }
    assert_eq!(model.snapshot()["offlineRecords"], 128);
    assert!(
        model
            .offline_start(json!({"connectorId":1,"idTag":"private-queue",
        "meterStart":0,"timestamp":"2026-09-01T00:00:00Z"}))
            .is_err()
    );
    let private: Value =
        serde_json::from_slice(&fs::read(&config.private_state_file).unwrap()).unwrap();
    assert_eq!(private["offline"][0]["start"]["meterStart"], i64::MAX - 1);
    assert_eq!(private["offline"][0]["stop"]["meterStop"], i64::MAX);
    assert_eq!(
        private["offline"][0]["stop"]["timestamp"],
        "2026-08-31T23:59:59Z"
    );
}
