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
fn schema_floors_admit_values_that_native_negotiation_semantics_refuse() {
    let cases: Value = serde_json::from_str(include_str!(
        "../corpus/wire/2.0.1/charging-negotiation-negative-cases.json"
    ))
    .unwrap();
    let cases = cases["cases"].as_array().unwrap();
    assert!(
        cases
            .iter()
            .filter(|case| case["schema_valid"] == true)
            .count()
            >= 10,
        "most refusals are native semantics the pinned schema floor does not express"
    );
    for case in cases {
        let action = case["wire"][2].as_str().unwrap();
        assert_eq!(
            schema(&format!("{action}Request")).is_valid(&case["wire"][3]),
            case["schema_valid"].as_bool().unwrap(),
            "{}",
            case["id"]
        );
        assert_eq!(case["native_valid"], false, "{}", case["id"]);
    }
    // The floor itself admits a CSO source; K11.FR.05/K12.FR.04 forbid it in Part 2 only.
    assert!(
        schema("NotifyChargingLimitRequest")
            .is_valid(&json!({"chargingLimit":{"chargingLimitSource":"CSO"}}))
    );
}

#[test]
fn native_answers_admit_exactly_the_pinned_status_sets() {
    let needs = schema("NotifyEVChargingNeedsResponse");
    for status in ["Accepted", "Rejected", "Processing"] {
        assert!(needs.is_valid(&json!({"status":status})));
        assert!(needs.is_valid(&json!({"status":status,"statusInfo":{"reasonCode":"NotEnabled"}})));
    }
    assert!(!needs.is_valid(&json!({"status":"Scheduled"})));
    assert!(
        !needs.is_valid(&json!({"status":"Rejected","statusInfo":{"reasonCode":"x".repeat(21)}}))
    );
    let schedule = schema("NotifyEVChargingScheduleResponse");
    for status in ["Accepted", "Rejected"] {
        assert!(schedule.is_valid(&json!({"status":status})));
    }
    assert!(!schedule.is_valid(&json!({"status":"Processing"})));
    for empty in [
        "NotifyChargingLimitResponse",
        "ClearedChargingLimitResponse",
    ] {
        let validator = schema(empty);
        assert!(validator.is_valid(&json!({})));
        assert!(
            !validator.is_valid(&json!({"status":"Accepted"})),
            "{empty} carries no status"
        );
    }
}
