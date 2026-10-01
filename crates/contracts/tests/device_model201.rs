use serde_json::json;
use uob_contracts::{CommandResult, DeviceModelQuery201};

#[test]
fn report_identity_public_schema_and_serde_preserve_signed_boundaries_and_reject_overflow() {
    let published: serde_json::Value =
        serde_json::from_str(include_str!("../schemas/v1.5/command-result.schema.json")).unwrap();
    let query_schema = json!({
        "$ref": "#/$defs/DeviceModelQuery201",
        "$defs": published["$defs"],
    });
    let validator = jsonschema::validator_for(&query_schema).unwrap();
    for template in [
        json!({"action":"GetReport","request_id":0,"selectors":[],"criteria":[]}),
        json!({"action":"GetBaseReport","request_id":0,"report_base":"FullInventory"}),
    ] {
        for request in [i32::MIN, -1, 0, i32::MAX] {
            let mut source = template.clone();
            source["request_id"] = json!(request);
            assert!(
                validator.is_valid(&source),
                "valid report identity: {source}"
            );
            let query: DeviceModelQuery201 = serde_json::from_value(source.clone()).unwrap();
            assert_eq!(query.request_id(), Some(request));
            assert_eq!(serde_json::to_value(query).unwrap(), source);
        }
        for request in [i64::from(i32::MIN) - 1, i64::from(i32::MAX) + 1] {
            let mut source = template.clone();
            source["request_id"] = json!(request);
            assert!(
                !validator.is_valid(&source),
                "out-of-range report identity: {source}"
            );
            assert!(serde_json::from_value::<DeviceModelQuery201>(source).is_err());
        }
    }
}

#[test]
fn historical_result_deserialization_does_not_invent_device_model_evidence() {
    let results: Vec<CommandResult> =
        serde_json::from_slice(include_bytes!("fixtures/command-results-v1.json")).unwrap();
    for result in results {
        assert!(result.device_model_201.is_none());
        assert!(
            serde_json::to_value(result)
                .unwrap()
                .get("device_model_201")
                .is_none()
        );
    }
}
