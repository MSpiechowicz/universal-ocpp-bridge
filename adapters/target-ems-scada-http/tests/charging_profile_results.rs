//! Test-only authoritative Target-origin results exercise actual scoped EMS reads, not native ingress grants.
#[allow(dead_code)]
#[path = "device_model201_results/fixtures.rs"]
mod fixtures;
#[path = "device_model201_results/host.rs"]
mod host;
#[path = "device_model201_results/schemas.rs"]
mod schemas;
use host::{Host, OPERATOR, READER, STATION_OPERATOR};
use serde_json::{Value, json};
use uob_contracts::{CommandResult, ContractVersion};

fn evidence() -> Value {
    json!({"action":"SetChargingProfile","request":{"connector_id":1,"cs_charging_profiles":{
        "charging_profile_id":-117,"stack_level":2,"charging_profile_purpose":"TxProfile","transaction_id":i32::MIN,
        "charging_profile_kind":"Relative","charging_schedule":{"charging_rate_unit":"W","charging_schedule_period":[{"start_period":0,"limit":"900719925474099.1","number_phases":4},{"start_period":60,"limit":"0"}]}
    }},"status":"Accepted"})
}
fn result(id: &str, origin: Value, station: &str, profile: Value) -> CommandResult {
    let mut result = fixtures::result(id, origin, station, fixtures::variables());
    result.device_model_201 = None;
    result.schema_version = ContractVersion::V1_CHARGING_PROFILE_16;
    let evidence: uob_contracts::ChargingProfileResult16 = serde_json::from_value(profile).unwrap();
    let accepted = evidence.accepted();
    result.lifecycle = uob_contracts::CommandLifecycle::ProtocolResponse {
        accepted,
        error: (!accepted).then_some(uob_contracts::CommandError {
            code: uob_contracts::CommandErrorCode::ProtocolRejected,
            detail: None,
        }),
    };
    result.charging_profile_16 = Some(evidence);
    result
}

#[tokio::test]
async fn current_profile_results_and_nested_exports_validate_while_historical_schema_stays_unchanged()
 {
    let clear = json!({"action":"ClearChargingProfile","request":{"id":-117,"connector_id":999,"charging_profile_purpose":"ChargePointMaxProfile","stack_level":0},"status":"Unknown"});
    let mut host = Host::start(
        vec![
            result(
                "set",
                fixtures::target_origin("operator", "main"),
                "station-a",
                evidence(),
            ),
            result(
                "clear",
                fixtures::target_origin("operator", "main"),
                "station-a",
                clear,
            ),
        ],
        true,
    )
    .await;
    let schemas = schemas::Schemas::fetch(&host).await;
    let current = schemas.canonical(schemas::RESULT);
    for id in ["set", "clear"] {
        let (status, body) = host
            .get(&format!("/bridge/v1/commands/{id}"), OPERATOR)
            .await;
        assert_eq!(status, 200);
        assert_eq!(body["schema_version"], json!({"major":1,"revision":6}));
        current.validate(&body).unwrap();
        schemas.status().validate(&body).unwrap();
        schemas.admitted().validate(&json!({"request_id":id,"status_url":format!("/bridge/v1/commands/{id}"),"result":body})).unwrap();
        schemas
            .canonical(schemas::RECORD)
            .validate(&fixtures::export_record(&body))
            .unwrap();
        schemas
            .canonical(schemas::BATCH)
            .validate(&fixtures::export_batch(&body))
            .unwrap();
    }
    let (_, body) = host.get("/bridge/v1/commands/set", OPERATOR).await;
    assert_eq!(
        body["charging_profile_16"]["request"]["cs_charging_profiles"]["charging_schedule"]["charging_schedule_period"]
            [0]["limit"],
        "900719925474099.1"
    );
    let (status, historical) = host
        .get("/bridge/v1/schemas/v1.5/command-result.schema.json", READER)
        .await;
    assert_eq!(status, 200);
    let historical = jsonschema::draft202012::new(&historical).unwrap();
    for invalid_profile in [
        json!({"action":"NotAProfileAction"}),
        json!({"action":"SetChargingProfile","request":{},"status":"Unknown"}),
        json!({"action":"ClearChargingProfile","request":{},"status":"NotSupported"}),
    ] {
        let mut invalid = body.clone();
        invalid["charging_profile_16"] = invalid_profile;
        assert!(!current.is_valid(&invalid));
        assert!(!schemas.status().is_valid(&invalid));
        assert!(historical.is_valid(&invalid));
        assert!(
            !schemas
                .canonical(schemas::RECORD)
                .is_valid(&fixtures::export_record(&invalid))
        );
    }
    host.stop().await;
}

#[tokio::test]
async fn profile_status_reads_require_control_exact_origin_target_and_resource_scope() {
    let cases = [
        (
            "own",
            fixtures::target_origin("operator", "main"),
            "station-a",
        ),
        (
            "management",
            json!({"kind":"management","principal_id":"operator"}),
            "station-a",
        ),
        (
            "foreign-target",
            fixtures::target_origin("operator", "other"),
            "station-a",
        ),
        (
            "foreign-principal",
            fixtures::target_origin("other", "main"),
            "station-a",
        ),
        (
            "station-own",
            fixtures::target_origin("station-operator", "main"),
            "station-a",
        ),
        (
            "station-outside",
            fixtures::target_origin("station-operator", "main"),
            "station-b",
        ),
    ];
    let mut host = Host::start(
        cases
            .into_iter()
            .map(|(id, origin, station)| result(id, origin, station, evidence()))
            .collect(),
        true,
    )
    .await;
    assert_eq!(host.get("/bridge/v1/commands/own", OPERATOR).await.0, 200);
    assert_eq!(host.get("/bridge/v1/commands/own", READER).await.0, 403);
    assert_eq!(
        host.get("/bridge/v1/commands/station-own", STATION_OPERATOR)
            .await
            .0,
        200
    );
    for (id, token) in [
        ("management", OPERATOR),
        ("foreign-target", OPERATOR),
        ("foreign-principal", OPERATOR),
        ("station-outside", STATION_OPERATOR),
    ] {
        assert_eq!(
            host.get(&format!("/bridge/v1/commands/{id}"), token).await,
            (404, json!({"error":"ems_scada_http.resource_not_found"}))
        );
    }
    // Existing EMS credentials do not acquire privileged profile command ingress.
    for action in ["SetChargingProfile", "ClearChargingProfile"] {
        let response=host.client.post(format!("{}/bridge/v1/commands",host.base)).bearer_auth(OPERATOR).json(&json!({"request_id":format!("denied-{action}"),"resource":{"bridge_id":"site-01","station_id":"station-a"},"operation":{"kind":"ocpp","parameters":{"protocol":"ocpp16j","action":action,"payload_schema":format!("urn:OCPP:1.6:2019:12:{action}Request"),"payload":{}}},"expires_at":"2099-01-01T00:00:00Z"})).send().await.unwrap();
        assert_eq!(response.status(), 403);
    }
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            host.driver.next_command()
        )
        .await
        .is_err()
    );
    host.stop().await;
}

#[tokio::test]
async fn rich_profile_result_keeps_existing_http_response_cap_without_truncation() {
    let mut large = result(
        "large",
        fixtures::target_origin("operator", "main"),
        "station-a",
        evidence(),
    );
    large.observed_effects=(0..2200).map(|index|serde_json::from_value(json!({"event_id":format!("independent-status-observation-{index}"),"event_type":"resource.status_changed","observed_at":"2026-09-01T12:00:00Z"})).unwrap()).collect();
    assert!(serde_json::to_vec(&large).unwrap().len() > 256 * 1024);
    let mut host = Host::start(
        vec![
            large,
            result(
                "small",
                fixtures::target_origin("operator", "main"),
                "station-a",
                evidence(),
            ),
        ],
        true,
    )
    .await;
    assert_eq!(
        host.get("/bridge/v1/commands/large", OPERATOR).await,
        (413, json!({"error":"ems_scada_http.payload_too_large"}))
    );
    let (status, body) = host.get("/bridge/v1/commands/small", OPERATOR).await;
    assert_eq!(status, 200);
    assert_eq!(body["charging_profile_16"]["status"], "Accepted");
    host.stop().await;
}
