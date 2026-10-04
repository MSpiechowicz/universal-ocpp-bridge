//! Explicit test-only target-owned result evidence tests consumption, not privileged target ingress.
#[allow(dead_code)]
#[path = "device_model201_results/fixtures.rs"]
mod fixtures;
#[path = "device_model201_results/host.rs"]
mod host;
#[path = "device_model201_results/schemas.rs"]
mod schemas;
use host::{Host, OPERATOR, READER};
use serde_json::{Value, json};
use uob_contracts::{
    CommandLifecycle, CommandResult, ContractVersion, LocalAuthorizationResult201,
};

fn result(id: &str, evidence: Value) -> CommandResult {
    let mut result = fixtures::result(
        id,
        fixtures::target_origin("operator", "main"),
        "station-a",
        fixtures::variables(),
    );
    result.device_model_201 = None;
    result.schema_version = ContractVersion::V1_LOCAL_AUTHORIZATION_201;
    let evidence: LocalAuthorizationResult201 = serde_json::from_value(evidence).unwrap();
    result.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted: evidence.accepted(),
        error: None,
    };
    result.local_authorization_201 = Some(evidence);
    result
}
#[tokio::test]
async fn served_native_201_results_validate_current_nested_contracts_without_private_fields() {
    let cases = [
        (
            "zero",
            json!({"action":"GetLocalListVersion","version_number":0}),
        ),
        (
            "installed",
            json!({"action":"GetLocalListVersion","version_number":7}),
        ),
        (
            "accepted",
            json!({"action":"SendLocalList","version_number":7,"update_type":"Full","status":"Accepted"}),
        ),
        (
            "failed",
            json!({"action":"SendLocalList","version_number":8,"update_type":"Differential","status":"Failed"}),
        ),
        (
            "mismatch",
            json!({"action":"SendLocalList","version_number":8,"update_type":"Differential","status":"VersionMismatch"}),
        ),
        (
            "cleared",
            json!({"action":"ClearCache","status":"Accepted"}),
        ),
        (
            "clear-rejected",
            json!({"action":"ClearCache","status":"Rejected"}),
        ),
    ];
    let mut host = Host::start(
        cases
            .iter()
            .map(|(id, evidence)| result(id, evidence.clone()))
            .collect(),
        true,
    )
    .await;
    let schemas = schemas::Schemas::fetch(&host).await;
    for (id, evidence) in cases {
        let (status, body) = host
            .get(&format!("/bridge/v1/commands/{id}"), OPERATOR)
            .await;
        assert_eq!(status, 200);
        assert_eq!(body["schema_version"]["revision"], 10);
        assert_eq!(body["local_authorization_201"], evidence);
        assert!(body["local_authorization_16"].is_null());
        let typed: CommandResult = serde_json::from_value(body.clone()).unwrap();
        assert!(
            typed.observed_effects.is_empty(),
            "a native list/cache ACK does not establish installed contents or physical effects",
        );
        schemas.canonical(schemas::RESULT).validate(&body).unwrap();
        schemas.status().validate(&body).unwrap();
        schemas
            .canonical(schemas::RECORD)
            .validate(&fixtures::export_record(&body))
            .unwrap();
        schemas
            .canonical(schemas::BATCH)
            .validate(&fixtures::export_batch(&body))
            .unwrap();
        for field in [
            "idToken",
            "groupIdToken",
            "personalMessage",
            "updateReference",
            "statusInfo",
            "customData",
            "localAuthorizationList",
        ] {
            let mut invalid = body.clone();
            invalid["local_authorization_201"][field] = json!("PRIVATE-MARKER-112");
            assert!(!schemas.status().is_valid(&invalid));
            assert!(serde_json::from_value::<CommandResult>(invalid).is_err());
        }
        assert_eq!(
            host.get(&format!("/bridge/v1/commands/{id}"), READER)
                .await
                .0,
            403
        );
    }
    let (_, mut invalid) = host.get("/bridge/v1/commands/accepted", OPERATOR).await;
    invalid["local_authorization_201"]["status"] = json!("NotSupported");
    assert!(!schemas.status().is_valid(&invalid));
    invalid["local_authorization_201"]["status"] = json!("Accepted");
    invalid["local_authorization_201"]["version_number"] = json!(0);
    assert!(!schemas.status().is_valid(&invalid));
    host.stop().await;
}
