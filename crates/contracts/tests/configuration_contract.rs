use serde_json::json;
use uob_contracts::{CommandResult, ConfigurationChangeReference, ConfigurationResult};

#[test]
fn protected_reference_envelope_rejects_inline_values_and_wrong_shapes() {
    let reference = format!("cfg:{}", "a".repeat(64));
    let valid = json!({"key":"HeartbeatInterval","valueReference":reference});
    let parsed: ConfigurationChangeReference = serde_json::from_value(valid.clone()).unwrap();
    assert!(ConfigurationChangeReference::valid_parts(
        &parsed.key,
        &parsed.value_reference
    ));
    assert!(ConfigurationChangeReference::valid_parts(
        "",
        &parsed.value_reference
    ));
    assert!(!ConfigurationChangeReference::valid_parts(
        &"x".repeat(51),
        &parsed.value_reference
    ));
    assert!(!ConfigurationChangeReference::valid_parts(
        "HeartbeatInterval",
        "cfg:weak"
    ));
    for invalid in [
        json!({"key":"HeartbeatInterval","value":"private-secret"}),
        json!({"key":"HeartbeatInterval","valueReference":reference,"value":"private-secret"}),
        json!({"key":"HeartbeatInterval"}),
    ] {
        assert!(serde_json::from_value::<ConfigurationChangeReference>(invalid).is_err());
    }
}

#[test]
fn earlier_command_result_schema_decodes_without_new_configuration_evidence() {
    let fixtures = include_str!("fixtures/command-results-v1.json");
    let results: Vec<CommandResult> = serde_json::from_str(fixtures).unwrap();
    assert!(results.iter().all(
        |result| result.configuration.is_none() && result.configuration_observations.is_empty()
    ));
    assert!(results.iter().all(|result| {
        serde_json::to_value(result)
            .unwrap()
            .get("configuration")
            .is_none()
    }));
}

#[test]
fn configuration_read_roundtrip_preserves_omitted_empty_and_partial_response_lists() {
    for (response, expected_keys, expected_unknown) in [
        (json!({}), None, None),
        (json!({"keys":[],"unknown_keys":[]}), Some(0), Some(0)),
        (json!({"unknown_keys":["Missing"]}), None, Some(1)),
        (json!({"keys":[]}), Some(0), None),
    ] {
        let mut encoded = response.clone();
        encoded["kind"] = json!("read");
        let parsed: ConfigurationResult = serde_json::from_value(encoded.clone()).unwrap();
        let ConfigurationResult::Read {
            requested_keys,
            keys,
            unknown_keys,
        } = &parsed
        else {
            panic!("read evidence")
        };
        assert!(requested_keys.is_none());
        assert_eq!(keys.as_ref().map(Vec::len), expected_keys);
        assert_eq!(unknown_keys.as_ref().map(Vec::len), expected_unknown);
        assert_eq!(serde_json::to_value(parsed).unwrap(), encoded);
    }
}
