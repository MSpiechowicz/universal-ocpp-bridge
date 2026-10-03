//! Test-only authoritative target-owned evidence, not privileged EMS native command dispatch.
#[path = "configuration201_results/boundaries.rs"]
mod boundaries;
#[allow(dead_code)]
#[path = "device_model201_results/fixtures.rs"]
mod fixtures;
#[path = "device_model201_results/host.rs"]
mod host;
#[path = "device_model201_results/schemas.rs"]
mod schemas;

use host::{Host, OPERATOR, READER};
use schemas::{BATCH, RECORD, RESULT, Schemas};
use serde_json::{Value, json};
use uob_contracts::{CommandLifecycle, CommandResult, ConfigurationResult201, ContractVersion};

fn variables(statuses: &[&str]) -> Value {
    json!({"action":"SetVariables", "variables":statuses.iter().enumerate().map(|(index, status)| {
        json!({"component":{"name":"VendorCtrlr","instance":"primary"},
            "variable":{"name":format!("Setting{index}")},
            "attribute_type":"Actual","status":status})
    }).collect::<Vec<_>>()})
}

fn network(status: &str) -> Value {
    json!({"action":"SetNetworkProfile","configuration_slot":0,
        "status":status,"staged":status == "Accepted"})
}

fn result(id: &str, origin: Value, station: &str, evidence: Value) -> CommandResult {
    let mut result = fixtures::result(id, origin, station, fixtures::variables());
    let typed: ConfigurationResult201 = serde_json::from_value(evidence).unwrap();
    result.device_model_201 = None;
    result.schema_version = ContractVersion::V1_CONFIGURATION_201;
    result.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted: typed.accepted(),
        error: None,
    };
    result.configuration_201 = Some(typed);
    result
}

fn own(id: &str, evidence: Value) -> CommandResult {
    result(
        id,
        fixtures::target_origin("operator", "main"),
        "station-a",
        evidence,
    )
}

#[tokio::test]
async fn served_configuration_results_validate_native_statuses_and_staged_not_active_exports() {
    let cases = [
        (
            "mixed",
            variables(&[
                "Accepted",
                "Rejected",
                "UnknownComponent",
                "UnknownVariable",
                "NotSupportedAttributeType",
                "RebootRequired",
            ]),
            false,
        ),
        ("reboot", variables(&["Accepted", "RebootRequired"]), true),
        ("staged", network("Accepted"), true),
        ("rejected", network("Rejected"), false),
        ("failed", network("Failed"), false),
    ];
    let mut host = Host::start(
        cases
            .iter()
            .map(|(id, evidence, _)| own(id, evidence.clone()))
            .collect(),
        true,
    )
    .await;
    let schemas = Schemas::fetch(&host).await;
    let current = schemas.canonical(RESULT);
    for (id, evidence, accepted) in cases {
        let (status, body) = host
            .get(&format!("/bridge/v1/commands/{id}"), OPERATOR)
            .await;
        assert_eq!(status, 200);
        assert_eq!(body["schema_version"], json!({"major":1,"revision":8}));
        assert_eq!(body["configuration_201"], evidence);
        assert_eq!(body["lifecycle"]["accepted"], accepted);
        let typed: CommandResult = serde_json::from_value(body.clone()).unwrap();
        assert!(
            typed.observed_effects.is_empty(),
            "a native ACK is not observed activation"
        );
        current.validate(&body).unwrap();
        schemas.status().validate(&body).unwrap();
        schemas.admitted().validate(&json!({"request_id":id,"status_url":format!("/bridge/v1/commands/{id}"),"result":body})).unwrap();
        schemas
            .canonical(RECORD)
            .validate(&fixtures::export_record(&body))
            .unwrap();
        schemas
            .canonical(BATCH)
            .validate(&fixtures::export_batch(&body))
            .unwrap();
        for field in [
            "attributeValue",
            "connectionData",
            "valueReference",
            "profileReference",
            "customData",
            "statusInfo",
            "active",
        ] {
            let mut invalid = body.clone();
            invalid["configuration_201"][field] = json!("synthetic-private-marker");
            assert!(!current.is_valid(&invalid), "{id} accepted {field}");
            assert!(
                !schemas
                    .canonical(BATCH)
                    .is_valid(&fixtures::export_batch(&invalid))
            );
        }
        let mut invalid = body;
        invalid["configuration_201"]["status"] = json!("UnknownStatus");
        assert!(!current.is_valid(&invalid));
    }
    // This historical route intentionally accepts unknown optional result extensions.
    let historical = "/bridge/v1/schemas/v1.7/command-result.schema.json";
    let (status, schema) = host.get(historical, READER).await;
    assert_eq!(status, 200);
    let old = jsonschema::draft202012::new(&schema).unwrap();
    let (_, mut legacy) = host.get("/bridge/v1/commands/staged", OPERATOR).await;
    legacy.as_object_mut().unwrap().remove("configuration_201");
    legacy["schema_version"]["revision"] = json!(7);
    old.validate(&legacy).unwrap();
    current.validate(&legacy).unwrap();
    assert!(
        serde_json::from_value::<CommandResult>(legacy.clone())
            .unwrap()
            .configuration_201
            .is_none()
    );
    legacy["configuration_201"] = json!({"action":"Invented"});
    assert!(old.is_valid(&legacy));
    assert!(!current.is_valid(&legacy));
    host.stop().await;
}

#[tokio::test]
async fn served_configuration_schema_rejects_item_secrets_and_invalid_native_slot_statuses() {
    let mut host = Host::start(
        vec![
            own("variables", variables(&["Accepted"])),
            own("network", network("Accepted")),
        ],
        true,
    )
    .await;
    let schemas = Schemas::fetch(&host).await;
    let validator = schemas.canonical(RESULT);
    let (_, variables) = host.get("/bridge/v1/commands/variables", OPERATOR).await;
    for field in [
        "attributeValue",
        "valueReference",
        "statusInfo",
        "customData",
    ] {
        let mut invalid = variables.clone();
        invalid["configuration_201"]["variables"][0][field] = json!("synthetic-private-marker");
        assert!(!validator.is_valid(&invalid));
        assert!(serde_json::from_value::<CommandResult>(invalid).is_err());
    }
    let (_, network) = host.get("/bridge/v1/commands/network", OPERATOR).await;
    for slot in [i64::from(i32::MIN), i64::from(i32::MAX)] {
        let mut valid = network.clone();
        valid["configuration_201"]["configuration_slot"] = json!(slot);
        validator.validate(&valid).unwrap();
    }
    for slot in [i64::from(i32::MIN) - 1, i64::from(i32::MAX) + 1] {
        let mut invalid = network.clone();
        invalid["configuration_201"]["configuration_slot"] = json!(slot);
        assert!(!validator.is_valid(&invalid));
    }
    for field in ["configuration_slot", "staged", "status"] {
        let mut invalid = network.clone();
        invalid["configuration_201"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(!validator.is_valid(&invalid));
    }
    let mut invalid = network;
    invalid["configuration_201"]["status"] = json!("RebootRequired");
    assert!(!validator.is_valid(&invalid));
    host.stop().await;
}
