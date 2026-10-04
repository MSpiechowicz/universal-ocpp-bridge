//! Consumer/version/authorization/bounds coverage through the real supervised EMS listener.
//! Results below are explicitly seeded test-only authoritative Target-origin state. These tests
//! do not dispatch privileged queries through production EMS credentials or relabel management
//! results; the privileged-ingress test instead proves those credentials remain unable to dispatch.

#[path = "device_model201_results/admission_schema.rs"]
mod admission_schema;
#[path = "device_model201_results/fixtures.rs"]
mod fixtures;
#[path = "device_model201_results/host.rs"]
mod host;
#[path = "device_model201_results/schemas.rs"]
mod schemas;

use std::time::Duration;

use fixtures::{base_report, complete_report, report, result, target_origin, variables};
use host::{Host, OPERATOR, READER, STATION_OPERATOR};
use schemas::{BATCH, HISTORICAL, RECORD, RESULT, Schemas};
use serde_json::{Value, json};

fn operator_origin() -> Value {
    target_origin("operator", "main")
}

#[tokio::test]
async fn served_current_contract_validates_device_states_and_rejects_invalid_native_fields() {
    let cases = [
        ("variables", variables()),
        ("pending", report(json!({"state":"pending"}), "Accepted")),
        ("complete", report(complete_report(2), "Accepted")),
        (
            "interrupted",
            base_report(
                json!({"state":"incomplete","reason":"interrupted","progress":null}),
                "Accepted",
            ),
        ),
        (
            "empty",
            base_report(json!({"state":"not_expected"}), "EmptyResultSet"),
        ),
    ];
    let seeded = cases
        .iter()
        .map(|(id, evidence)| result(id, operator_origin(), "station-a", evidence.clone()))
        .collect();
    let mut host = Host::start(seeded, true).await;
    let schemas = Schemas::fetch(&host).await;
    let current = schemas.canonical(RESULT);
    let status_schema = schemas.status();
    let admitted_schema = schemas.admitted();

    for (id, _) in cases {
        let (status, body) = host
            .get(&format!("/bridge/v1/commands/{id}"), OPERATOR)
            .await;
        assert_eq!(status, 200, "{id}: {body}");
        assert_eq!(body["schema_version"], json!({"major":1,"revision":5}));
        current.validate(&body).unwrap();
        status_schema.validate(&body).unwrap();
        admitted_schema
            .validate(&json!({
                "request_id":id, "status_url":format!("/bridge/v1/commands/{id}"),
                "result":body
            }))
            .unwrap();
    }

    let (_, mut invalid_status) = host.get("/bridge/v1/commands/variables", OPERATOR).await;
    invalid_status["device_model_201"]["variables"][0]["status"] = json!("UnknownStatus");
    assert!(!current.is_valid(&invalid_status));
    let (_, complete) = host.get("/bridge/v1/commands/complete", OPERATOR).await;
    assert_eq!(complete["device_model_201"]["query"]["request_id"], -109);
    assert_eq!(complete["device_model_201"]["native_ack"], "Accepted");
    assert_eq!(complete["device_model_201"]["report"]["state"], "complete");
    assert_eq!(
        complete["device_model_201"]["report"]["fragments"][0]["sequence"],
        0
    );
    let private_attribute = &complete["device_model_201"]["report"]["items"][0]["attributes"][0];
    assert_eq!(private_attribute["value"]["redacted"], true);
    assert!(private_attribute["value"].get("value").is_none());

    // A stale consumer reference to v1.4 would accept this extension without inspecting it.
    let mut invalid = complete.clone();
    invalid["device_model_201"]
        .as_object_mut()
        .unwrap()
        .remove("connection");
    assert!(!current.is_valid(&invalid), "missing connection accepted");
    assert!(
        !status_schema.is_valid(&invalid),
        "stale status schema reference"
    );
    assert!(
        !admitted_schema.is_valid(&json!({
            "request_id":"complete", "status_url":"/bridge/v1/commands/complete",
            "result":invalid
        })),
        "stale admitted-result schema reference"
    );

    for native_id in [i64::from(i32::MIN), i64::from(i32::MAX)] {
        let mut boundary = complete.clone();
        boundary["device_model_201"]["query"]["request_id"] = json!(native_id);
        assert!(current.is_valid(&boundary));
    }
    for native_id in [i64::from(i32::MIN) - 1, i64::from(i32::MAX) + 1] {
        let mut invalid = complete.clone();
        invalid["device_model_201"]["query"]["request_id"] = json!(native_id);
        assert!(!current.is_valid(&invalid));
    }
    host.stop().await;
}

#[tokio::test]
async fn historical_schema_remains_discoverable_and_export_schemas_validate_new_nested_evidence() {
    let mut host = Host::start(
        vec![result(
            "export",
            operator_origin(),
            "station-a",
            report(complete_report(1), "Accepted"),
        )],
        true,
    )
    .await;
    let schemas = Schemas::fetch(&host).await;
    let historical = schemas.canonical(HISTORICAL);
    let current = schemas.canonical(RESULT);
    let (_, body) = host.get("/bridge/v1/commands/export", OPERATOR).await;
    let mut legacy = body.clone();
    legacy.as_object_mut().unwrap().remove("device_model_201");
    legacy["schema_version"]["revision"] = json!(4);
    historical.validate(&legacy).unwrap();
    current.validate(&legacy).unwrap();

    // Historical v1.4 intentionally knows nothing about device evidence. Replacing its route
    // with the new schema changes this observable extension behavior.
    legacy["device_model_201"] = json!({"query":{"action":"NotAnOcppAction"}});
    assert!(historical.is_valid(&legacy));
    assert!(!current.is_valid(&legacy));

    let record_validator = schemas.canonical(RECORD);
    let batch_validator = schemas.canonical(BATCH);
    record_validator
        .validate(&fixtures::export_record(&body))
        .unwrap();
    batch_validator
        .validate(&fixtures::export_batch(&body))
        .unwrap();
    let mut invalid = body;
    invalid["device_model_201"]["report"]["progress"]["fragments"] = json!(-1);
    assert!(!record_validator.is_valid(&fixtures::export_record(&invalid)));
    assert!(!batch_validator.is_valid(&fixtures::export_batch(&invalid)));
    host.stop().await;
}

#[tokio::test]
async fn device_evidence_status_requires_control_resource_scope_and_exact_target_origin() {
    let seeded = [
        ("own", operator_origin(), "station-a"),
        (
            "management",
            json!({"kind":"management","principal_id":"operator"}),
            "station-a",
        ),
        (
            "foreign-target",
            target_origin("operator", "other-target"),
            "station-a",
        ),
        (
            "foreign-principal",
            target_origin("other-principal", "main"),
            "station-a",
        ),
        (
            "station-own",
            target_origin("station-operator", "main"),
            "station-a",
        ),
        (
            "station-outside",
            target_origin("station-operator", "main"),
            "station-b",
        ),
        ("host-outside", operator_origin(), "ungranted-station"),
    ]
    .into_iter()
    .map(|(id, origin, station)| result(id, origin, station, variables()))
    .collect();
    let mut host = Host::start(seeded, true).await;
    assert_eq!(host.get("/bridge/v1/commands/own", OPERATOR).await.0, 200);
    assert_eq!(host.get("/bridge/v1/commands/own", READER).await.0, 403);
    assert_eq!(
        host.get("/bridge/v1/commands/station-own", STATION_OPERATOR)
            .await
            .0,
        200
    );
    for (id, credential) in [
        ("management", OPERATOR),
        ("foreign-target", OPERATOR),
        ("foreign-principal", OPERATOR),
        ("station-outside", STATION_OPERATOR),
        ("host-outside", OPERATOR),
    ] {
        let (status, body) = host
            .get(&format!("/bridge/v1/commands/{id}"), credential)
            .await;
        assert_eq!(status, 404, "{id}: {body}");
        assert_eq!(body, json!({"error":"ems_scada_http.resource_not_found"}));
    }
    host.stop().await;

    let mut ungranted = Host::start(
        vec![result("own", operator_origin(), "station-a", variables())],
        false,
    )
    .await;
    let (status, body) = ungranted.get("/bridge/v1/commands/own", OPERATOR).await;
    // With no host CommandStatus grant the operation itself is unsupported, before a result
    // can be read. This is distinct from non-disclosing origin/resource failures above.
    assert_eq!(status, 501);
    assert_eq!(body["error"], "ems_scada_http.operation_not_supported");
    assert!(body.get("device_model_201").is_none());
    assert!(body.get("result").is_none());
    ungranted.stop().await;
}

#[tokio::test]
async fn privileged_device_queries_remain_rejected_before_target_command_admission() {
    let mut host = Host::start(vec![], true).await;
    for (action, payload) in [
        (
            "GetVariables",
            json!({"getVariableData":[{
                "component":{"name":"DeviceDataCtrlr"},
                "variable":{"name":"ItemsPerMessage","instance":"GetVariables"}
            }]}),
        ),
        (
            "GetBaseReport",
            json!({"requestId":109,"reportBase":"FullInventory"}),
        ),
        ("GetReport", json!({"requestId":-109})),
    ] {
        let response = host
            .client
            .post(format!("{}/bridge/v1/commands", host.base))
            .bearer_auth(OPERATOR)
            .json(&json!({
                "request_id":format!("denied-{action}"),
                "resource":{"bridge_id":"site-01","station_id":"station-a"},
                "operation":{"kind":"ocpp","parameters":{
                    "protocol":"ocpp201","action":action,
                    "payload_schema":format!("urn:OCPP:Cp:2:2020:3:{action}Request"),
                    "payload":payload
                }},
                "expires_at":"2099-01-01T00:00:00Z"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403, "{action}");
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"error":"ems_scada_http.permission_denied"})
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), host.driver.next_command())
            .await
            .is_err()
    );
    host.stop().await;
}

#[tokio::test]
async fn oversized_rich_report_is_an_explicit_error_without_truncated_success_or_larger_caps() {
    let oversized = result(
        "large",
        operator_origin(),
        "station-a",
        report(complete_report(1024), "Accepted"),
    );
    let bytes = serde_json::to_vec(&oversized).unwrap().len();
    assert!(bytes > 256 * 1024 && bytes < 1024 * 1024);
    let mut host = Host::start(
        vec![
            oversized,
            result(
                "small",
                operator_origin(),
                "station-a",
                report(complete_report(1), "Accepted"),
            ),
        ],
        true,
    )
    .await;
    let (_, capabilities) = host.get("/bridge/v1/capabilities", OPERATOR).await;
    assert_eq!(capabilities["limits"]["maximum_message_bytes"], 256 * 1024);
    assert_eq!(capabilities["limits"]["maximum_request_bytes"], 64 * 1024);
    let (status, body) = host.get("/bridge/v1/commands/large", OPERATOR).await;
    assert_eq!(status, 413);
    assert_eq!(body, json!({"error":"ems_scada_http.payload_too_large"}));
    let (status, small) = host.get("/bridge/v1/commands/small", OPERATOR).await;
    assert_eq!(status, 200);
    assert_eq!(small["device_model_201"]["report"]["state"], "complete");
    assert_eq!(small["device_model_201"]["report"]["progress"]["items"], 1);
    host.stop().await;
}
