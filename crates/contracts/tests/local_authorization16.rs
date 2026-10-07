use serde_json::{Value, json};
use uob_contracts::*;

fn envelope(version: &Value) -> Value {
    json!({"listVersion":version,"updateType":"Full","updateReference":format!("list16:{}", "a".repeat(64))})
}
#[test]
fn protected_envelope_rejects_inline_material_and_only_native_sentinels() {
    for version in [i32::MIN, -2, 1, i32::MAX] {
        let request: SendLocalListReference16 =
            serde_json::from_value(envelope(&json!(version))).unwrap();
        assert_eq!(request.list_version, version);
    }
    for version in [
        json!(-1),
        json!(0),
        json!(2_147_483_648_u64),
        json!(-2_147_483_649_i64),
        json!(1.5),
    ] {
        assert!(serde_json::from_value::<SendLocalListReference16>(envelope(&version)).is_err());
    }
    for field in ["localAuthorizationList", "idTag", "parentIdTag", "extra"] {
        let mut raw = envelope(&json!(1));
        raw[field] = json!("PRIVATE-MARKER");
        assert!(serde_json::from_value::<SendLocalListReference16>(raw).is_err());
    }
    for capability in [
        "sha256:secret",
        "list16:0",
        "cfg201:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        let mut raw = envelope(&json!(1));
        raw["updateReference"] = json!(capability);
        assert!(serde_json::from_value::<SendLocalListReference16>(raw).is_err());
    }
}
#[test]
fn old_results_decode_absent_evidence_and_native_results_survive_nested_exports() {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/command-results-v1.json")).unwrap();
    let old: CommandResult = serde_json::from_value(fixtures[1].clone()).unwrap();
    assert!(old.local_authorization_16.is_none());
    let published: Value =
        serde_json::from_str(include_str!("../schemas/v1.9/command-result.schema.json")).unwrap();
    let validator = jsonschema::draft202012::new(&published).unwrap();
    let mut result = old;
    result.schema_version = ContractVersion::V1_LOCAL_AUTHORIZATION_16;
    for evidence in [
        LocalAuthorizationResult16::GetLocalListVersion { list_version: -1 },
        LocalAuthorizationResult16::GetLocalListVersion { list_version: 0 },
        LocalAuthorizationResult16::SendLocalList {
            list_version: -2,
            update_type: LocalListUpdateType16::Full,
            status: SendLocalListStatus16::VersionMismatch,
        },
        LocalAuthorizationResult16::ClearCache {
            status: ClearCacheStatus16::Rejected,
        },
    ] {
        result.lifecycle = CommandLifecycle::ProtocolResponse {
            accepted: evidence.accepted(),
            error: None,
        };
        result.local_authorization_16 = Some(evidence.clone());
        let encoded = serde_json::to_value(&result).unwrap();
        assert!(validator.is_valid(&encoded));
        assert_eq!(
            serde_json::from_value::<CommandResult>(encoded)
                .unwrap()
                .local_authorization_16,
            Some(evidence)
        );
        let batch: ExportBatch =
            serde_json::from_str(include_str!("fixtures/export-batch-v1.json")).unwrap();
        let record = ExportRecord::new(
            batch.records()[0].metadata().clone(),
            ExportPayload::CommandResult(result.clone()),
        );
        assert_eq!(record.metadata().schema_version.revision, 16);
        let export_schema: Value =
            serde_json::from_str(include_str!("../schemas/v1.10/export-record.schema.json"))
                .unwrap();
        assert!(
            jsonschema::draft202012::new(&export_schema)
                .unwrap()
                .is_valid(&serde_json::to_value(record).unwrap())
        );
    }
}
#[test]
fn public_evidence_never_accepts_native_private_fields() {
    let raw = json!({"action":"SendLocalList","list_version":1,"update_type":"Full","status":"Accepted","idTag":"PRIVATE-MARKER"});
    assert!(serde_json::from_value::<LocalAuthorizationResult16>(raw).is_err());
    let schema: Value = serde_json::from_str(include_str!(
        "../schemas/v1.0/send-local-list-reference-16.schema.json"
    ))
    .unwrap();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    assert!(validator.is_valid(&envelope(&json!(-2))));
    assert!(!validator.is_valid(&envelope(&json!(0))));
}
