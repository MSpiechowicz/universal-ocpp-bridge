use jsonschema::Draft;
use serde_json::{Value, json};

fn schema(name: &str) -> jsonschema::Validator {
    let root = uob_ocpp_fixtures::corpus_root()
        .join("schemas/2.0.1")
        .join(name);
    let schema: Value = serde_json::from_slice(&std::fs::read(root).unwrap()).unwrap();
    jsonschema::options()
        .with_draft(Draft::Draft6)
        .build(&schema)
        .unwrap()
}
#[test]
fn pinned_native_empty_array_is_not_omission_and_schema_floor_is_not_semantic_acceptance() {
    let validator = schema("SendLocalListRequest.json");
    for update_type in ["Full", "Differential"] {
        assert!(validator.is_valid(&json!({"versionNumber":1,"updateType":update_type})));
        assert!(!validator.is_valid(
            &json!({"versionNumber":1,"updateType":update_type,"localAuthorizationList":[]})
        ));
    }
    // Part 2 printed395 adds a Full-only conditional requirement that the
    // unchanged Part 3 JSON schema intentionally cannot express.
    assert!(validator.is_valid(&json!({"versionNumber":20,"updateType":"Full","localAuthorizationList":[{"idToken":{"idToken":"native-other-112","type":"Local"}}]})));
    let corpus: Value = serde_json::from_str(include_str!(
        "../corpus/wire/2.0.1/local-list-negative-cases.json"
    ))
    .unwrap();
    for case in corpus["cases"].as_array().unwrap() {
        assert_eq!(
            validator.is_valid(&case["wire"][3]),
            case["schema_valid"].as_bool().unwrap(),
            "{}",
            case["id"]
        );
    }
}
#[test]
fn pinned_native_responses_never_invent_not_supported_or_legacy_version_fields() {
    let send = schema("SendLocalListResponse.json");
    for status in ["Accepted", "Failed", "VersionMismatch"] {
        assert!(send.is_valid(&json!({"status":status})));
    }
    assert!(!send.is_valid(&json!({"status":"NotSupported"})));
    let query = schema("GetLocalListVersionResponse.json");
    assert!(query.is_valid(&json!({"versionNumber":0})));
    assert!(!query.is_valid(&json!({"listVersion":-1})));
    let clear = schema("ClearCacheResponse.json");
    for status in ["Accepted", "Rejected"] {
        assert!(clear.is_valid(&json!({"status":status})));
    }
    assert!(!clear.is_valid(&json!({"status":"Failed"})));
}

#[test]
fn independently_authored_native_controller_report_has_distinct_count_and_capacity() {
    let report: Value = serde_json::from_str(include_str!(
        "../corpus/wire/2.0.1/notify-report-local-authorization.json"
    ))
    .unwrap();
    assert!(schema("NotifyReportRequest.json").is_valid(&report[3]));
    let entries = &report[3]["reportData"][0];
    assert_eq!(entries["variableAttribute"][0]["type"], "Actual");
    assert_eq!(entries["variableAttribute"][0]["value"], "1");
    assert_eq!(entries["variableCharacteristics"]["maxLimit"], 256);
    let authorize: Value =
        serde_json::from_str(include_str!("../corpus/wire/2.0.1/authorize.json")).unwrap();
    assert!(schema("AuthorizeRequest.json").is_valid(&authorize[3]));
    assert_eq!(authorize[2], "Authorize");
    assert_eq!(authorize[3]["idToken"]["type"], "Local");
}

#[test]
fn pinned_evse_integer_schema_floor_does_not_establish_positive_identifier_semantics() {
    let validator = schema("SendLocalListRequest.json");
    for ids in [json!([0]), json!([-1]), json!([1, 0]), json!([1, -1])] {
        assert!(validator.is_valid(&json!({
            "versionNumber":8,"updateType":"Full","localAuthorizationList":[{
                "idToken":{"idToken":"synthetic-evse-floor","type":"Local"},
                "idTokenInfo":{"status":"Accepted","evseId":ids}
            }]
        })));
    }
}
