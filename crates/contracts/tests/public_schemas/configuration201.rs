use super::*;
use uob_contracts::{ConfigurationResult201, ExportPayload};

fn result(evidence: Value) -> Value {
    let fixtures: Value =
        serde_json::from_str(include_str!("../fixtures/command-results-v1.json")).unwrap();
    let mut result = fixtures[1].clone();
    result["schema_version"]["revision"] = json!(8);
    let typed: ConfigurationResult201 = serde_json::from_value(evidence.clone()).unwrap();
    result["lifecycle"] = json!({"stage":"protocol_response","accepted":typed.accepted()});
    result["configuration_201"] = evidence;
    result
}

fn variable(status: &str, index: usize) -> Value {
    json!({"component":{"name":"VendorCtrlr","instance":"primary"},
        "variable":{"name":format!("Setting{index}")},
        "attribute_type":"Actual","status":status})
}

#[test]
fn configuration_results_and_nested_exports_preserve_native_statuses_without_values() {
    let statuses = [
        "Accepted",
        "Rejected",
        "UnknownComponent",
        "UnknownVariable",
        "NotSupportedAttributeType",
        "RebootRequired",
    ];
    let mixed = result(json!({"action":"SetVariables","variables":statuses.iter()
        .enumerate().map(|(index, status)|variable(status, index)).collect::<Vec<_>>()}));
    assert_eq!(mixed["lifecycle"]["accepted"], false);
    let reboot = result(json!({"action":"SetVariables",
        "variables":[variable("Accepted", 0), variable("RebootRequired", 1)]}));
    assert_eq!(reboot["lifecycle"]["accepted"], true);
    let mut cases = vec![mixed, reboot];
    for status in ["Accepted", "Rejected", "Failed"] {
        cases.push(result(
            json!({"action":"SetNetworkProfile","configuration_slot":0,
            "status":status,"staged":status == "Accepted"}),
        ));
    }
    for result in cases {
        assert_valid("command-result", &result);
        let typed: CommandResult = serde_json::from_value(result.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(&typed).unwrap()["configuration_201"],
            result["configuration_201"]
        );
        assert!(typed.observed_effects.is_empty());
        let mut batch: ExportBatch =
            serde_json::from_str(include_str!("../fixtures/export-batch-v1.json")).unwrap();
        let record = ExportRecord::new(
            batch.records()[0].metadata().clone(),
            ExportPayload::CommandResult(typed),
        );
        assert_eq!(record.metadata().schema_version.revision, 16);
        assert_eq!(ExportBatch::SCHEMA_VERSION.revision, 16);
        batch = ExportBatch::new(
            batch.batch_id().clone(),
            batch.destination().clone(),
            vec![record],
        )
        .unwrap();
        let encoded = serde_json::to_value(&batch).unwrap();
        assert_valid("export-record", &encoded["records"][0]);
        assert_valid("export-batch", &encoded);
        for field in [
            "attributeValue",
            "connectionData",
            "valueReference",
            "profileReference",
            "statusInfo",
            "customData",
        ] {
            let mut invalid = result.clone();
            invalid["configuration_201"][field] = json!("synthetic-private-marker");
            assert!(
                !jsonschema::draft202012::new(&published("command-result"))
                    .unwrap()
                    .is_valid(&invalid)
            );
            assert!(serde_json::from_value::<CommandResult>(invalid.clone()).is_err());
            let mut invalid_batch = encoded.clone();
            invalid_batch["records"][0]["payload"]["data"] = invalid;
            assert!(
                !jsonschema::draft202012::new(&published("export-batch"))
                    .unwrap()
                    .is_valid(&invalid_batch)
            );
            assert!(serde_json::from_value::<ExportBatch>(invalid_batch).is_err());
        }
    }
    let mut invalid =
        result(json!({"action":"SetVariables","variables":[variable("Accepted", 0)]}));
    invalid["configuration_201"]["variables"][0]["status"] = json!("Failed");
    assert!(
        !jsonschema::draft202012::new(&published("command-result"))
            .unwrap()
            .is_valid(&invalid)
    );
}

#[test]
fn configuration_evidence_is_additive_and_historical_results_remain_readable() {
    for (name, previous) in [
        (
            "command-result",
            include_str!("../../schemas/v1.7/command-result.schema.json"),
        ),
        (
            "export-record",
            include_str!("../../schemas/v1.8/export-record.schema.json"),
        ),
        (
            "export-batch",
            include_str!("../../schemas/v1.8/export-batch.schema.json"),
        ),
    ] {
        let old: Value = serde_json::from_str(previous).unwrap();
        let new = published(name);
        assert!(compatibility_errors(&old, &new, "$").is_empty());
        for (definition, previous) in old["$defs"].as_object().unwrap() {
            assert!(
                compatibility_errors(previous, &new["$defs"][definition], definition).is_empty()
            );
        }
    }
    let fixtures: Value =
        serde_json::from_str(include_str!("../fixtures/command-results-v1.json")).unwrap();
    for historical in fixtures.as_array().unwrap() {
        let typed: CommandResult = serde_json::from_value(historical.clone()).unwrap();
        assert_eq!(typed.configuration_201, None);
        assert!(
            serde_json::to_value(typed)
                .unwrap()
                .get("configuration_201")
                .is_none()
        );
        assert_valid("command-result", historical);
    }
}

#[test]
fn protected_reference_schemas_reject_native_secrets_and_freeform_extensions() {
    for (name, envelope, secret_field) in [
        (
            "set-variables-reference-201",
            json!({"setVariableData":[{
            "component":{"name":"VendorCtrlr"},"variable":{"name":"PrivateSetting"},
            "valueReference":format!("cfg201:{}", "a".repeat(64))}]}),
            "attributeValue",
        ),
        (
            "set-network-profile-reference-201",
            json!({"configurationSlot":0,
            "profileReference":format!("cfg201:{}", "b".repeat(64))}),
            "connectionData",
        ),
    ] {
        assert_valid(name, &envelope);
        let schema = published(name);
        assert_eq!(
            schema["x-uob-contract-version"],
            json!({"major":1,"revision":0})
        );
        let validator = jsonschema::draft202012::new(&schema).unwrap();
        for field in [secret_field, "customData"] {
            let mut invalid = envelope.clone();
            if name == "set-variables-reference-201" {
                invalid["setVariableData"][0][field] = json!("synthetic-private-marker");
                assert!(
                    serde_json::from_value::<SetVariablesReference201>(invalid.clone()).is_err()
                );
            } else {
                invalid[field] = json!("synthetic-private-marker");
                assert!(
                    serde_json::from_value::<SetNetworkProfileReference201>(invalid.clone())
                        .is_err()
                );
            }
            assert!(!validator.is_valid(&invalid));
        }
    }
}

#[test]
fn protected_network_profile_schema_preserves_the_full_signed_slot_boundary() {
    let validator =
        jsonschema::draft202012::new(&published("set-network-profile-reference-201")).unwrap();
    let envelope = json!({
        "configurationSlot": 0,
        "profileReference": format!("cfg201:{}", "c".repeat(64))
    });
    for slot in [i64::from(i32::MIN), 0, i64::from(i32::MAX)] {
        let mut valid = envelope.clone();
        valid["configurationSlot"] = json!(slot);
        validator.validate(&valid).unwrap();
        serde_json::from_value::<SetNetworkProfileReference201>(valid).unwrap();
    }
    for slot in [i64::from(i32::MIN) - 1, i64::from(i32::MAX) + 1] {
        let mut invalid = envelope.clone();
        invalid["configurationSlot"] = json!(slot);
        assert!(
            !validator.is_valid(&invalid),
            "out-of-range native slot {slot}"
        );
        assert!(serde_json::from_value::<SetNetworkProfileReference201>(invalid).is_err());
    }
}
