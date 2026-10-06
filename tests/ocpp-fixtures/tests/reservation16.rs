use jsonschema::Draft;
use serde_json::{Value, json};

fn schema(name: &str) -> jsonschema::Validator {
    let root = uob_ocpp_fixtures::corpus_root()
        .join("schemas/1.6")
        .join(format!("{name}.json"));
    let source: Value = serde_json::from_slice(&std::fs::read(root).unwrap()).unwrap();
    jsonschema::options()
        .with_draft(Draft::Draft4)
        .should_validate_formats(true)
        .build(&source)
        .unwrap()
}

#[test]
fn signed_identity_and_native_schema_floor_are_independent() {
    let validator = schema("ReserveNow");
    for id in [i32::MIN, -113, 0, i32::MAX] {
        assert!(validator.is_valid(&json!({"connectorId":0,"expiryDate":"2099-01-01T00:00:00Z","idTag":"native","reservationId":id})));
        assert!(schema("CancelReservation").is_valid(&json!({"reservationId":id})));
    }
    let cases: Value = serde_json::from_str(include_str!(
        "../corpus/wire/1.6/reservation-negative-cases.json"
    ))
    .unwrap();
    for case in cases["cases"].as_array().unwrap() {
        assert_eq!(
            validator.is_valid(&case["wire"][3]),
            case["schema_valid"].as_bool().unwrap(),
            "{}",
            case["id"]
        );
    }
    assert!(!schema("CancelReservation").is_valid(&json!({"reservationId":0,"connectorId":1})));
}

#[test]
fn exact_status_sets_do_not_allow_cache_information_or_synthetic_success() {
    let reserve = schema("ReserveNowResponse");
    for status in ["Accepted", "Faulted", "Occupied", "Rejected", "Unavailable"] {
        assert!(reserve.is_valid(&json!({"status":status})));
        assert!(!reserve.is_valid(&json!({"status":status,"idTagInfo":{"status":"Accepted"}})));
    }
    assert!(!reserve.is_valid(&json!({"status":"Scheduled"})));
    let cancel = schema("CancelReservationResponse");
    assert!(cancel.is_valid(&json!({"status":"Accepted"})));
    assert!(cancel.is_valid(&json!({"status":"Rejected"})));
    assert!(!cancel.is_valid(&json!({"status":"Unknown"})));
}
