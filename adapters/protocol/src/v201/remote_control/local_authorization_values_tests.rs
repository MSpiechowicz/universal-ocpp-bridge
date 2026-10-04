use super::*;
use serde_json::json;

fn native() -> Value {
    json!({"versionNumber":1,"updateType":"Full","localAuthorizationList":[{
        "idToken":{"idToken":"Native-Token","type":"Local","additionalInfo":[{"additionalIdToken":"PRIVATE-EXTRA","type":"UTF8-żółć"}]},
        "idTokenInfo":{"status":"Accepted","chargingPriority":9,"language1":"en","language2":"pl",
            "cacheExpiryDateTime":"2099-01-01T00:00:00.123Z","evseId":[1],
            "groupIdToken":{"idToken":"PRIVATE-GROUP","type":"Local"},
            "personalMessage":{"format":"UTF8","content":"PRIVATE-MESSAGE"},
            "customData":{"vendorId":"vendor","nested":{"private":"PRIVATE-NESTED"}}}
    }]})
}
fn protected(value: &Value) -> Result<ProtectedLocalListUpdate201, StationCommandError> {
    ProtectedLocalListUpdate201::from_json_bytes(value.to_string().into_bytes())
}
#[test]
fn valid_native_metadata_is_preserved_privately_without_normalizing_wire_identities() {
    let value = native();
    let update = protected(&value).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(update.raw().get()).unwrap(),
        value
    );
    assert_eq!(update.count, 1);
    assert_eq!(update.upsert_count, 1);
    assert!(update.has_expiry);
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
        let mut value = native();
        value["localAuthorizationList"][0]["idTokenInfo"]["status"] = json!(status);
        assert!(protected(&value).is_ok());
    }
}
#[test]
fn typed_casefold_rejects_duplicates_but_not_different_token_types() {
    let mut value = native();
    let mut second = value["localAuthorizationList"][0].clone();
    second["idToken"]["idToken"] = json!("NATIVE-TOKEN");
    value["localAuthorizationList"]
        .as_array_mut()
        .unwrap()
        .push(second);
    assert!(protected(&value).is_err());
    value["localAuthorizationList"][1]["idToken"]["type"] = json!("KeyCode");
    assert!(protected(&value).is_ok());
}
#[test]
fn omission_is_valid_empty_is_invalid_and_native_metadata_errors_are_value_free() {
    for update_type in ["Full", "Differential"] {
        assert!(protected(&json!({"versionNumber":1,"updateType":update_type})).is_ok());
        assert!(
            protected(
                &json!({"versionNumber":1,"updateType":update_type,"localAuthorizationList":[]})
            )
            .is_err()
        );
    }
    for mutation in 0..13 {
        let mut value = native();
        match mutation {
            0 => value["versionNumber"] = json!(0),
            1 => value["versionNumber"] = json!(-1),
            2 => value["versionNumber"] = json!(2_147_483_648_u64),
            3 => value["localAuthorizationList"][0]["idToken"]["type"] = json!("PRIVATE-BAD-TYPE"),
            4 => {
                value["localAuthorizationList"][0]["idTokenInfo"]["status"] =
                    json!("PRIVATE-BAD-STATUS");
            }
            5 => value["localAuthorizationList"][0]["idTokenInfo"]["chargingPriority"] = json!(10),
            6 => {
                value["localAuthorizationList"][0]["idTokenInfo"]["cacheExpiryDateTime"] =
                    json!("2099-01-01T00:00:00.1234Z");
            }
            7 => {
                value["localAuthorizationList"][0]["idTokenInfo"]["evseId"] =
                    json!([2_147_483_648_u64]);
            }
            8 => value["localAuthorizationList"][0]["idTokenInfo"]["language2"] = json!("en"),
            9 => {
                value["localAuthorizationList"][0]["idTokenInfo"]["personalMessage"]["format"] =
                    json!("PRIVATE-FORMAT");
            }
            10 => {
                value["localAuthorizationList"][0]["idTokenInfo"]
                    .as_object_mut()
                    .unwrap()
                    .remove("language1");
            }
            11 => {
                value["localAuthorizationList"][0]["idTokenInfo"]["language1"] = json!("PRIVATE_");
            }
            _ => value["localAuthorizationList"][0]["idTokenInfo"]["language2"] = json!("EN"),
        }
        let error = protected(&value).err().expect("invalid metadata");
        for marker in ["PRIVATE", "Native-Token", "native-token"] {
            assert!(!error.context().contains(marker));
        }
    }
}
#[test]
fn entry_and_retained_material_caps_fail_closed() {
    let entries: Vec<_> = (0..257)
        .map(|index| json!({"idToken":{"idToken":index.to_string(),"type":"Local"}}))
        .collect();
    assert!(
        protected(
            &json!({"versionNumber":1,"updateType":"Differential","localAuthorizationList":entries})
        )
        .is_err()
    );
    let mut value = native();
    value["customData"] = json!({"vendorId":"vendor","private":"s".repeat(65_536)});
    assert!(protected(&value).is_err());
}
#[test]
fn identifier_string_charset_applies_only_to_primary_group_and_additional_identifiers() {
    for text in ["", "aZ09*-_=:+|@.", "Native-Token"] {
        let mut value = native();
        value["localAuthorizationList"][0]["idToken"]["idToken"] = json!(text);
        assert!(protected(&value).is_ok());
    }
    for text in ["Straße", "with space", "slash/token", "\"escaped\""] {
        for field in 0..3 {
            let mut value = native();
            let item = &mut value["localAuthorizationList"][0];
            match field {
                0 => item["idToken"]["idToken"] = json!(text),
                1 => item["idTokenInfo"]["groupIdToken"]["idToken"] = json!(text),
                _ => item["idToken"]["additionalInfo"][0]["additionalIdToken"] = json!(text),
            }
            assert!(protected(&value).is_err());
        }
    }
}
#[test]
fn no_authorization_requires_empty_without_banning_empty_other_native_types() {
    let mut value = native();
    value["localAuthorizationList"][0]["idToken"]["type"] = json!("NoAuthorization");
    assert!(protected(&value).is_err());
    value["localAuthorizationList"][0]["idToken"]["idToken"] = json!("");
    assert!(protected(&value).is_ok());
    value["localAuthorizationList"][0]["idToken"]["type"] = json!("Central");
    assert!(protected(&value).is_ok());
}

#[test]
fn full_requires_info_but_differential_absence_is_a_private_deletion() {
    let mut value = native();
    value["localAuthorizationList"][0]
        .as_object_mut()
        .unwrap()
        .remove("idTokenInfo");
    let error = protected(&value).err().expect("Full entry requires info");
    for marker in ["PRIVATE", "Native-Token", "native-token"] {
        assert!(!error.context().contains(marker));
    }
    value["updateType"] = json!("Differential");
    let deletion = protected(&value).expect("Differential deletion");
    assert_eq!(deletion.count, 1);
    assert_eq!(deletion.upsert_count, 0);
    assert_eq!(
        serde_json::from_str::<Value>(deletion.raw().get()).unwrap(),
        value
    );
    let omitted = protected(&json!({"versionNumber":1,"updateType":"Full"}))
        .expect("omitted Full list clears");
    assert_eq!(omitted.count, 0);
    assert_eq!(omitted.upsert_count, 0);
}

#[test]
fn differential_tracks_unique_upserts_separately_from_deletions() {
    let mut value = native();
    value["updateType"] = json!("Differential");
    value["localAuthorizationList"].as_array_mut().unwrap().extend([
        json!({"idToken":{"idToken":"second","type":"Local"},"idTokenInfo":{"status":"Blocked"}}),
        json!({"idToken":{"idToken":"deleted","type":"Local"}}),
    ]);
    let update = protected(&value).expect("mixed Differential");
    assert_eq!(update.count, 3);
    assert_eq!(update.upsert_count, 2);
}
