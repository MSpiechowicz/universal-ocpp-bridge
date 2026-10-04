use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uob_sim::local_authorization::LocalAuthorizationConfig;
use uob_sim::local_authorization201::{IdToken, IdTokenInfo, LocalAuthorization201Handle};

fn full() -> Value {
    let wire: Value = serde_json::from_str(include_str!(
        "../../../tests/ocpp-fixtures/corpus/wire/2.0.1/local-list-full.json"
    ))
    .unwrap();
    wire[3].clone()
}
fn token(value: &str, kind: &str) -> IdToken {
    serde_json::from_value(json!({"idToken":value,"type":kind})).unwrap()
}
fn info(status: &str) -> IdTokenInfo {
    serde_json::from_value(json!({"status":status})).unwrap()
}
fn now() -> OffsetDateTime {
    OffsetDateTime::parse("2026-10-04T00:00:00Z", &Rfc3339).unwrap()
}
#[test]
fn full_diff_delete_omission_versions_and_typed_casefold_are_atomic() {
    let model = LocalAuthorization201Handle::in_memory("alpha");
    assert_eq!(model.snapshot()["listVersion"], 0);
    assert_eq!(model.update(&full()), "Accepted");
    assert!(model.authorize_offline(&token("list-marker-112", "Central"), 1, now()));
    assert!(!model.authorize_offline(&token("list-marker-112", "Local"), 1, now()));
    assert!(!model.authorize_offline(&token("list-marker-112", "Central"), 2, now()));
    assert_eq!(
        model.update(&json!({"versionNumber":8,"updateType":"Differential"})),
        "Accepted"
    );
    assert_eq!(model.snapshot()["listEntries"], 1);
    assert_eq!(
        model.update(&json!({"versionNumber":8,"updateType":"Differential"})),
        "VersionMismatch"
    );
    assert_eq!(model.update(&json!({"versionNumber":9,"updateType":"Differential","localAuthorizationList":[{"idToken":{"idToken":"LiSt-MaRkEr-112","type":"Central"}}]})), "Accepted");
    assert_eq!(model.snapshot()["listEntries"], 0);
    assert_eq!(model.snapshot()["listVersion"], 9);
    assert_eq!(
        model.update(&json!({"versionNumber":1,"updateType":"Full"})),
        "Accepted"
    );
    assert_eq!(model.snapshot()["listVersion"], 1);
    let mut native_ascii = full();
    native_ascii["localAuthorizationList"][0]["idToken"]["idToken"] = json!("A*-_=:+|@.");
    assert_eq!(model.update(&native_ascii), "Accepted");
    assert!(model.authorize_offline(&token("a*-_=:+|@.", "Central"), 1, now()));
    for invalid in ["Straße", "é", "e\u{301}", "with space", "slash/token"] {
        native_ascii["localAuthorizationList"][0]["idToken"]["idToken"] = json!(invalid);
        assert_eq!(model.update(&native_ascii), "Failed");
    }
}
#[test]
fn invalid_empty_duplicate_oversize_and_metadata_never_mutate() {
    let model = LocalAuthorization201Handle::in_memory("alpha");
    assert_eq!(model.update(&full()), "Accepted");
    let prior = model.snapshot();
    let mut invalid = Vec::new();
    for version in [json!(0), json!(-1), json!(2_147_483_648_i64)] {
        let mut value = full();
        value["versionNumber"] = version;
        invalid.push(value);
    }
    for contents in [json!([]), json!(null)] {
        let mut value = full();
        value["localAuthorizationList"] = contents;
        invalid.push(value);
    }
    let mut duplicate = full();
    let mut entry = duplicate["localAuthorizationList"][0].clone();
    entry["idToken"]["idToken"] = json!("list-marker-112");
    duplicate["localAuthorizationList"]
        .as_array_mut()
        .unwrap()
        .push(entry);
    invalid.push(duplicate);
    for (field, value) in [
        ("status", json!("Invented")),
        ("chargingPriority", json!(10)),
        ("cacheExpiryDateTime", json!("2099-01-01T00:00:00.1234Z")),
        ("evseId", json!([])),
        ("language2", json!("en")),
    ] {
        let mut update = full();
        update["localAuthorizationList"][0]["idTokenInfo"][field] = value;
        invalid.push(update);
    }
    let mut too_many = full();
    too_many["localAuthorizationList"] = json!((0..257).map(|n| json!({"idToken":{"idToken":format!("id-{n}"),"type":"Local"},"idTokenInfo":{"status":"Accepted"}})).collect::<Vec<_>>());
    invalid.push(too_many);
    for update in invalid {
        assert_eq!(model.update(&update), "Failed");
        assert_eq!(model.snapshot(), prior);
    }
}

#[test]
fn nonpositive_evse_metadata_rejects_full_and_diff_without_partial_installation() {
    let model = LocalAuthorization201Handle::in_memory("alpha");
    assert_eq!(model.update(&full()), "Accepted");
    let prior = model.snapshot();
    for update_type in ["Full", "Differential"] {
        for ids in [
            json!([0]),
            json!([-1]),
            json!([1, 0]),
            json!([1, -1]),
            json!([-2_147_483_648_i64]),
        ] {
            let mut update = full();
            update["versionNumber"] = json!(8);
            update["updateType"] = json!(update_type);
            update["localAuthorizationList"][0]["idTokenInfo"]["status"] = json!("Blocked");
            update["localAuthorizationList"][0]["idTokenInfo"]["evseId"] = ids;
            update["localAuthorizationList"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "idToken":{"idToken":"otherwise-valid-new-entry","type":"Local"},
                    "idTokenInfo":{"status":"Accepted","evseId":[1]}
                }));
            assert_eq!(model.update(&update), "Failed");
            assert_eq!(model.snapshot(), prior);
            assert!(model.authorize_offline(&token("list-marker-112", "Central"), 1, now()));
            assert!(!model.authorize_offline(
                &token("otherwise-valid-new-entry", "Local"),
                1,
                now()
            ));
        }
    }
    let mut valid = full();
    valid["localAuthorizationList"][0]["idTokenInfo"]["evseId"] = json!([1, 2_147_483_647]);
    assert_eq!(model.update(&valid), "Accepted");
}
#[test]
fn clear_is_cache_only_and_list_denial_expiry_prioritize_over_cached_acceptance() {
    let model = LocalAuthorization201Handle::in_memory("alpha");
    model
        .observe_central(token("LIST-MARKER-112", "Central"), info("Accepted"))
        .unwrap();
    let mut blocked = full();
    blocked["localAuthorizationList"][0]["idTokenInfo"]["status"] = json!("Blocked");
    assert_eq!(model.update(&blocked), "Accepted");
    assert!(!model.authorize_offline(&token("list-marker-112", "Central"), 1, now()));
    let mut expired = full();
    expired["localAuthorizationList"][0]["idTokenInfo"]["cacheExpiryDateTime"] =
        json!("2026-10-04T00:00:00Z");
    assert_eq!(model.update(&expired), "Accepted");
    assert!(!model.authorize_offline(&token("list-marker-112", "Central"), 1, now()));
    assert_eq!(model.update(&json!({"versionNumber":8,"updateType":"Differential","localAuthorizationList":[{"idToken":{"idToken":"list-marker-112","type":"Central"}}]})), "Accepted");
    assert!(model.authorize_offline(&token("list-marker-112", "Central"), 1, now()));
    assert!(model.clear_cache());
    assert_eq!(model.snapshot()["listVersion"], 8);
    assert!(!model.authorize_offline(&token("list-marker-112", "Central"), 1, now()));
    assert_eq!(model.update(&full()), "Accepted");
    assert!(model.clear_cache());
    assert!(model.authorize_offline(&token("list-marker-112", "Central"), 1, now()));
}
#[test]
fn durable_native_metadata_binding_permissions_disabled_cache_and_failed_io() {
    let directory = std::env::temp_dir().join(format!("native201-model-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let config = LocalAuthorizationConfig {
        private_state_file: directory.join("private.json").display().to_string(),
        list_supported: true,
        cache_supported: true,
    };
    let model = LocalAuthorization201Handle::open("alpha", &config).unwrap();
    assert_eq!(model.update(&full()), "Accepted");
    let retained: Value =
        serde_json::from_slice(&fs::read(&config.private_state_file).unwrap()).unwrap();
    assert_eq!(
        retained["list"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap(),
        &full()["localAuthorizationList"][0]
    );
    assert_eq!(
        fs::metadata(&config.private_state_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(LocalAuthorization201Handle::open("alpha", &config).is_err());
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(
        model.update(&json!({"versionNumber":1,"updateType":"Full"})),
        "Failed"
    );
    assert!(!model.clear_cache());
    assert_eq!(model.snapshot()["listVersion"], 7);
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    drop(model);
    assert!(LocalAuthorization201Handle::open("other", &config).is_err());
    let mut disabled = config.clone();
    disabled.cache_supported = false;
    let recovered = LocalAuthorization201Handle::open("alpha", &disabled).unwrap();
    assert!(!recovered.clear_cache());
    assert!(recovered.authorize_offline(&token("list-marker-112", "Central"), 1, now()));
    drop(recovered);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn independently_authored_negative_corpus_preserves_installed_native_state() {
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../tests/ocpp-fixtures/corpus/wire/2.0.1/local-list-negative-cases.json"
    ))
    .unwrap();
    let model = LocalAuthorization201Handle::in_memory("alpha");
    assert_eq!(model.update(&full()), "Accepted");
    let prior = model.snapshot();
    for case in corpus["cases"].as_array().unwrap() {
        assert_eq!(
            model.update(&case["wire"][3]),
            case["simulator_status"].as_str().unwrap(),
            "{}",
            case["id"]
        );
        assert_eq!(model.snapshot(), prior, "{}", case["id"]);
    }
}

#[test]
fn all_native_statuses_and_valid_languages_metadata_are_private_and_inert() {
    let model = LocalAuthorization201Handle::in_memory("alpha");
    for status in [
        "Accepted",
        "Blocked",
        "ConcurrentTx",
        "Expired",
        "Invalid",
        "NoCredit",
        "NotAllowedTypeEVSE",
        "NotAtThisLocation",
        "NotAtThisTime",
        "Unknown",
    ] {
        let mut update = full();
        update["localAuthorizationList"][0]["idTokenInfo"]["status"] = json!(status);
        assert_eq!(model.update(&update), "Accepted");
        assert_eq!(
            model.authorize_offline(&token("list-marker-112", "Central"), 1, now()),
            status == "Accepted"
        );
        assert!(!model.snapshot().to_string().contains("MARKER-112"));
    }
    for language in [
        "en", "pl", "en-US", "zh-Hans", "i-ami", "x-uob", "de-1996", "en-u-ca",
    ] {
        let mut update = full();
        update["localAuthorizationList"][0]["idTokenInfo"]["language1"] = json!(language);
        update["localAuthorizationList"][0]["idTokenInfo"]
            .as_object_mut()
            .unwrap()
            .remove("language2");
        assert_eq!(model.update(&update), "Accepted");
        assert!(model.authorize_offline(&token("list-marker-112", "Central"), 1, now()));
    }
    let mut empty = full();
    empty["localAuthorizationList"][0]["idToken"] = json!({"idToken":"","type":"NoAuthorization"});
    assert_eq!(model.update(&empty), "Accepted");
    assert!(model.authorize_offline(&token("", "NoAuthorization"), 1, now()));
}

#[test]
fn disabled_list_reports_zero_retains_private_contents_and_reveals_independent_cache() {
    let directory = std::env::temp_dir().join(format!("native201-toggle-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let config = LocalAuthorizationConfig {
        private_state_file: directory.join("private.json").display().to_string(),
        list_supported: true,
        cache_supported: true,
    };
    let model = LocalAuthorization201Handle::open("alpha", &config).unwrap();
    model
        .observe_central(token("list-marker-112", "Central"), info("Accepted"))
        .unwrap();
    let mut blocked = full();
    blocked["localAuthorizationList"][0]["idTokenInfo"]["status"] = json!("Blocked");
    assert_eq!(model.update(&blocked), "Accepted");
    assert_eq!(model.snapshot()["cacheEntries"], 1);
    assert!(!model.authorize_offline(&token("list-marker-112", "Central"), 1, now()));
    drop(model);
    let mut disabled = config.clone();
    disabled.list_supported = false;
    let recovered = LocalAuthorization201Handle::open("alpha", &disabled).unwrap();
    assert_eq!(recovered.snapshot()["listVersion"], 0);
    assert_eq!(recovered.snapshot()["listEntries"], 1);
    assert!(recovered.authorize_offline(&token("list-marker-112", "Central"), 2, now()));
    assert_eq!(recovered.update(&full()), "Failed");
    drop(recovered);
    let enabled = LocalAuthorization201Handle::open("alpha", &config).unwrap();
    assert_eq!(enabled.snapshot()["listVersion"], 7);
    assert!(!enabled.authorize_offline(&token("list-marker-112", "Central"), 1, now()));
    drop(enabled);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn full_entry_requires_information_while_omitted_full_contents_clear() {
    let model = LocalAuthorization201Handle::in_memory("alpha");
    assert_eq!(model.update(&full()), "Accepted");
    let before = model.snapshot();
    let malformed = json!({"versionNumber":20,"updateType":"Full","localAuthorizationList":[{"idToken":{"idToken":"native-other-112","type":"Local"}}]});
    assert_eq!(model.update(&malformed), "Failed");
    assert_eq!(model.snapshot(), before);
    assert!(model.authorize_offline(&token("list-marker-112", "Central"), 1, now()));
    assert!(!model.authorize_offline(&token("native-other-112", "Local"), 1, now()));
    assert_eq!(
        model.update(&json!({"versionNumber":1,"updateType":"Full"})),
        "Accepted"
    );
    assert_eq!(model.snapshot()["listVersion"], 1);
    assert_eq!(model.snapshot()["listEntries"], 0);
}

#[test]
fn identifier_primitives_remain_ascii_while_valid_generic_private_metadata_is_utf8() {
    let model = LocalAuthorization201Handle::in_memory("alpha");
    let mut payload = full();
    payload["localAuthorizationList"][0]["idToken"]["additionalInfo"][0]["type"] =
        json!("ważny-café");
    payload["localAuthorizationList"][0]["idTokenInfo"]["personalMessage"]["content"] =
        json!("Żółw café — private");
    assert_eq!(model.update(&payload), "Accepted");
    assert!(model.authorize_offline(&token("list-marker-112", "Central"), 1, now()));
    let before = model.snapshot();
    payload["versionNumber"] = json!(8);
    payload["localAuthorizationList"][0]["idToken"]["additionalInfo"][0]["additionalIdToken"] =
        json!("żółw");
    assert_eq!(model.update(&payload), "Failed");
    assert_eq!(model.snapshot(), before);
    payload = full();
    payload["localAuthorizationList"][0]["idTokenInfo"]["language1"] = json!("en");
    payload["localAuthorizationList"][0]["idTokenInfo"]["language2"] = json!("EN");
    assert_eq!(model.update(&payload), "Failed");
    assert_eq!(model.snapshot(), before);
}
