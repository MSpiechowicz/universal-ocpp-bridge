use serde_json::{Value, json};
use uob_contracts::*;

#[test]
fn protected_envelopes_reject_raw_material_and_unknown_fields() {
    let reference = format!("cfg201:{}", "a".repeat(64));
    let valid = json!({"setVariableData":[{"component":{"name":"Vendor"},"variable":{"name":"Secret"},"valueReference":reference}]});
    assert!(serde_json::from_value::<SetVariablesReference201>(valid.clone()).is_ok());
    let mut raw = valid.clone();
    raw["setVariableData"][0]["attributeValue"] = json!("DO_NOT_PERSIST");
    assert!(serde_json::from_value::<SetVariablesReference201>(raw).is_err());
    assert!(
        serde_json::from_value::<SetNetworkProfileReference201>(
            json!({"configurationSlot":0,"connectionData":{"secret":"DO_NOT_PERSIST"}})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<SetNetworkProfileReference201>(
            json!({"configurationSlot":0,"profileReference":reference})
        )
        .is_ok()
    );
    assert!(valid_configuration_reference_201(&reference));
    assert!(!valid_configuration_reference_201("cfg201:short"));
}

#[test]
fn aggregate_acceptance_preserves_mixed_and_reboot_required_outcomes() {
    let statuses = [
        SetVariableStatus201::Accepted,
        SetVariableStatus201::Rejected,
        SetVariableStatus201::UnknownComponent,
        SetVariableStatus201::UnknownVariable,
        SetVariableStatus201::NotSupportedAttributeType,
        SetVariableStatus201::RebootRequired,
    ];
    for status in statuses {
        let item = SetVariableResult201 {
            component: DeviceComponent201 {
                name: "Vendor".into(),
                instance: None,
                evse: None,
            },
            variable: DeviceVariable201 {
                name: "Value".into(),
                instance: None,
            },
            attribute_type: DeviceAttributeType201::Actual,
            status,
        };
        let evidence = ConfigurationResult201::SetVariables {
            variables: vec![item],
        };
        assert_eq!(
            evidence.accepted(),
            matches!(
                status,
                SetVariableStatus201::Accepted | SetVariableStatus201::RebootRequired
            )
        );
        assert_eq!(
            serde_json::from_value::<ConfigurationResult201>(
                serde_json::to_value(&evidence).unwrap()
            )
            .unwrap(),
            evidence
        );
    }
    for status in [
        SetNetworkProfileStatus201::Accepted,
        SetNetworkProfileStatus201::Rejected,
        SetNetworkProfileStatus201::Failed,
    ] {
        let evidence = ConfigurationResult201::SetNetworkProfile {
            configuration_slot: 0,
            status,
            staged: status == SetNetworkProfileStatus201::Accepted,
        };
        assert_eq!(
            evidence.accepted(),
            status == SetNetworkProfileStatus201::Accepted
        );
        let mut invalid = serde_json::to_value(&evidence).unwrap();
        invalid["profileReference"] = json!("FORBIDDEN");
        assert!(serde_json::from_value::<ConfigurationResult201>(invalid).is_err());
    }
}

#[test]
fn historical_results_remain_readable_and_new_evidence_has_no_secret_surface() {
    let historical: Vec<Value> =
        serde_json::from_slice(include_bytes!("fixtures/command-results-v1.json")).unwrap();
    let mut result = historical[1].clone();
    assert!(
        serde_json::from_value::<CommandResult>(result.clone())
            .unwrap()
            .configuration_201
            .is_none()
    );
    result["schema_version"] = json!({"major":1,"revision":8});
    result["configuration_201"] = json!({"action":"SetNetworkProfile","configuration_slot":0,"status":"Accepted","staged":true});
    let typed: CommandResult = serde_json::from_value(result).unwrap();
    assert_eq!(typed.schema_version, ContractVersion::V1_CONFIGURATION_201);
    assert_eq!(
        typed.configuration_201.unwrap(),
        ConfigurationResult201::SetNetworkProfile {
            configuration_slot: 0,
            status: SetNetworkProfileStatus201::Accepted,
            staged: true
        }
    );
}
