use serde_json::json;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uob_sim::local_authorization201::{IdToken, IdTokenInfo, LocalAuthorization201Handle};

fn token(value: &str) -> IdToken {
    serde_json::from_value(json!({"idToken":value,"type":"Local"})).unwrap()
}
fn info(status: &str) -> IdTokenInfo {
    serde_json::from_value(json!({"status":status})).unwrap()
}
fn now() -> OffsetDateTime {
    OffsetDateTime::parse("2026-10-04T00:00:00Z", &Rfc3339).unwrap()
}

#[test]
fn latest_nonaccepted_information_refreshes_even_a_list_known_identity() {
    let model = LocalAuthorization201Handle::in_memory("alpha");
    let full = json!({"versionNumber":7,"updateType":"Full","localAuthorizationList":[{"idToken":{"idToken":"same-native-id","type":"Local"},"idTokenInfo":{"status":"Accepted"}}]});
    assert_eq!(model.update(&full), "Accepted");
    model
        .observe_central(token("SAME-NATIVE-ID"), info("Accepted"))
        .unwrap();
    assert_eq!(model.snapshot()["cacheEntries"], 1);
    model
        .observe_central(token("same-native-id"), info("Blocked"))
        .unwrap();
    assert!(model.authorize_offline(&token("same-native-id"), 1, now()));
    assert_eq!(model.update(&json!({"versionNumber":8,"updateType":"Differential","localAuthorizationList":[{"idToken":{"idToken":"same-native-id","type":"Local"}}]})),"Accepted");
    assert!(!model.authorize_offline(&token("same-native-id"), 1, now()));
    assert_eq!(model.snapshot()["cacheEntries"], 1);
    model
        .observe_central(token("same-native-id"), info("Accepted"))
        .unwrap();
    assert!(model.authorize_offline(&token("same-native-id"), 1, now()));
    model
        .observe_central(token("same-native-id"), info("Invalid"))
        .unwrap();
    assert!(!model.authorize_offline(&token("same-native-id"), 1, now()));
}

#[test]
fn full_cache_accepts_new_identity_by_evicting_the_older_received_entry() {
    let model = LocalAuthorization201Handle::in_memory("alpha");
    for index in 0..256 {
        model
            .observe_central(token(&format!("cache-{index}")), info("Accepted"))
            .unwrap();
    }
    model
        .observe_central(token("cache-0"), info("Blocked"))
        .unwrap();
    model
        .observe_central(token("new-cache-id"), info("Accepted"))
        .unwrap();
    assert_eq!(model.snapshot()["cacheEntries"], 256);
    assert!(!model.authorize_offline(&token("cache-1"), 1, now()));
    assert!(!model.authorize_offline(&token("cache-0"), 1, now()));
    assert!(model.authorize_offline(&token("cache-2"), 1, now()));
    assert!(model.authorize_offline(&token("new-cache-id"), 1, now()));
    model
        .observe_central(token("cache-0"), info("Accepted"))
        .unwrap();
    assert!(model.authorize_offline(&token("cache-0"), 1, now()));
    assert_eq!(model.snapshot()["cacheEntries"], 256);
}

#[test]
fn explicit_native_cache_expiry_removes_acceptance_without_an_invented_lifetime() {
    let model = LocalAuthorization201Handle::in_memory("alpha");
    model
        .observe_central(token("expiry-id"), info("Accepted"))
        .unwrap();
    let expired: IdTokenInfo = serde_json::from_value(
        json!({"status":"Accepted","cacheExpiryDateTime":"2000-01-01T00:00:00.123Z"}),
    )
    .unwrap();
    model.observe_central(token("expiry-id"), expired).unwrap();
    assert_eq!(model.snapshot()["cacheEntries"], 0);
    assert!(!model.authorize_offline(&token("expiry-id"), 1, now()));
    let future: IdTokenInfo = serde_json::from_value(
        json!({"status":"Accepted","cacheExpiryDateTime":"2099-01-01T00:00:00Z"}),
    )
    .unwrap();
    model.observe_central(token("expiry-id"), future).unwrap();
    let later = OffsetDateTime::parse("2100-01-01T00:00:00Z", &Rfc3339).unwrap();
    assert!(!model.authorize_offline(&token("expiry-id"), 1, later));
    assert_eq!(model.snapshot()["cacheEntries"], 0);
    model
        .observe_central(token("no-expiry-id"), info("Accepted"))
        .unwrap();
    assert!(model.authorize_offline(&token("no-expiry-id"), 1, later));
}

#[test]
fn a_new_denial_that_cannot_be_persisted_never_leaves_old_acceptance_usable() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use uob_sim::local_authorization::LocalAuthorizationConfig;
    let directory = std::env::temp_dir().join(format!("native-cache-io-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let config = LocalAuthorizationConfig {
        private_state_file: directory.join("private.json").display().to_string(),
        list_supported: true,
        cache_supported: true,
    };
    let model = LocalAuthorization201Handle::open("alpha", &config).unwrap();
    model
        .observe_central(token("received-latest-id"), info("Accepted"))
        .unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
    assert!(
        model
            .observe_central(token("received-latest-id"), info("Blocked"))
            .is_err()
    );
    assert_eq!(model.snapshot()["stateAvailable"], false);
    assert!(!model.authorize_offline(&token("received-latest-id"), 1, now()));
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    drop(model);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn invalid_central_evse_metadata_neither_refreshes_nor_inserts_cache_information() {
    let model = LocalAuthorization201Handle::in_memory("alpha");
    model
        .observe_central(token("prior-valid-id"), info("Accepted"))
        .unwrap();
    let prior = model.snapshot();
    for ids in [
        json!([0]),
        json!([-1]),
        json!([1, 0]),
        json!([1, -1]),
        json!([-2_147_483_648_i64]),
    ] {
        let invalid: IdTokenInfo =
            serde_json::from_value(json!({"status":"Blocked","evseId":ids})).unwrap();
        assert_eq!(
            model.observe_central(token("prior-valid-id"), invalid.clone()),
            Err("invalid_native_info")
        );
        assert_eq!(
            model.observe_central(token("new-invalid-id"), invalid),
            Err("invalid_native_info")
        );
        assert_eq!(model.snapshot(), prior);
        assert!(model.authorize_offline(&token("prior-valid-id"), 1, now()));
        assert!(!model.authorize_offline(&token("new-invalid-id"), 1, now()));
    }
}
