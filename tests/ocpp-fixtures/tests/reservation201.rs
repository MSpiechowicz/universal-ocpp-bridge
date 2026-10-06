use jsonschema::Draft;
use serde_json::{Value, json};

fn schema(name: &str) -> jsonschema::Validator {
    let root = uob_ocpp_fixtures::corpus_root()
        .join("schemas/2.0.1")
        .join(format!("{name}.json"));
    let source: Value = serde_json::from_slice(&std::fs::read(root).unwrap()).unwrap();
    jsonschema::options()
        .with_draft(Draft::Draft6)
        .should_validate_formats(true)
        .build(&source)
        .unwrap()
}

#[test]
fn schema_floor_admits_wider_values_than_the_native_reservation_contract() {
    let validator = schema("ReserveNowRequest");
    for id in [i32::MIN, -114, 0, i32::MAX] {
        assert!(validator.is_valid(&json!({"id":id,"expiryDateTime":"2099-01-01T00:00:00Z","idToken":{"idToken":"native","type":"ISO14443"}})));
        assert!(schema("CancelReservationRequest").is_valid(&json!({"reservationId":id})));
    }
    let cases: Value = serde_json::from_str(include_str!(
        "../corpus/wire/2.0.1/reservation-negative-cases.json"
    ))
    .unwrap();
    let cases = cases["cases"].as_array().unwrap();
    assert!(
        cases
            .iter()
            .any(|case| case["schema_valid"] == true && case["native_valid"] == false),
        "the corpus records values the pinned schema floor admits but native semantics refuse"
    );
    for case in cases {
        assert_eq!(
            validator.is_valid(&case["wire"][3]),
            case["schema_valid"].as_bool().unwrap(),
            "{}",
            case["id"]
        );
        assert_eq!(case["native_valid"], false, "{}", case["id"]);
    }
    assert!(
        !schema("CancelReservationRequest").is_valid(&json!({"reservationId":0,"evseId":1})),
        "cancellation never carries a scope"
    );
}

#[test]
fn exact_status_sets_and_native_updates_admit_no_synthetic_or_inferred_state() {
    let reserve = schema("ReserveNowResponse");
    for status in ["Accepted", "Faulted", "Occupied", "Rejected", "Unavailable"] {
        assert!(reserve.is_valid(&json!({"status":status})));
        assert!(reserve.is_valid(&json!({"status":status,"statusInfo":{"reasonCode":"Code"}})));
        assert!(!reserve.is_valid(&json!({"status":status,"idTokenInfo":{"status":"Accepted"}})));
    }
    assert!(!reserve.is_valid(&json!({"status":"Scheduled"})));
    assert!(
        !reserve.is_valid(&json!({"status":"Accepted","statusInfo":{"reasonCode":"x".repeat(21)}}))
    );
    let cancel = schema("CancelReservationResponse");
    assert!(cancel.is_valid(&json!({"status":"Accepted"})));
    assert!(cancel.is_valid(&json!({"status":"Rejected"})));
    assert!(!cancel.is_valid(&json!({"status":"Occupied"})));
    let update = schema("ReservationStatusUpdateRequest");
    for status in ["Expired", "Removed"] {
        assert!(update.is_valid(&json!({"reservationId":-114,"reservationUpdateStatus":status})));
    }
    for status in ["Faulted", "Unavailable", "Cancelled", "Consumed"] {
        assert!(
            !update.is_valid(&json!({"reservationId":-114,"reservationUpdateStatus":status})),
            "{status} is not a native reservation update"
        );
    }
    assert!(schema("ReservationStatusUpdateResponse").is_valid(&json!({})));
    let transaction = schema("TransactionEventRequest");
    let wire: Value = serde_json::from_str(include_str!(
        "../corpus/wire/2.0.1/reservation-transaction-tokenless.json"
    ))
    .unwrap();
    assert!(transaction.is_valid(&wire[3]));
    assert!(wire[3]["idToken"].is_null() && wire[3]["reservationId"] == -114);
}
