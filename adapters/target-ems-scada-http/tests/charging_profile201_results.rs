//! Retained test-only Target-origin evidence exercises production scoped reads, not native ingress.
#[allow(dead_code)]
#[path = "device_model201_results/fixtures.rs"]
mod fixtures;
#[path = "device_model201_results/host.rs"]
mod host;
#[path = "device_model201_results/schemas.rs"]
mod schemas;
use host::{Host, OPERATOR, READER, STATION_OPERATOR};
use serde_json::{Value, json};
use uob_contracts::{ChargingProfileResult201, CommandLifecycle, CommandResult, ContractVersion};

fn evidence(periods: usize) -> Value {
    assert!((2..=1024).contains(&periods));
    let periods: Vec<_> = (0..periods).map(|index| {
        if index == 0 {
            json!({"start_period":0,"limit":"900719925474099.1","number_phases":1,"phase_to_use":2})
        } else {
            json!({"start_period":index * 60,"limit":"0"})
        }
    }).collect();
    json!({"action":"SetChargingProfile","request":{"evse_id":2,"charging_profile":{
        "id":-118,"stack_level":2,"charging_profile_purpose":"TxProfile",
        "charging_profile_kind":"Relative","transaction_id":"native-TX-118",
        "charging_schedule":[{"id":-8,"charging_rate_unit":"W","min_charging_rate":"0",
            "charging_schedule_period":periods}]
    }},"status":"Accepted"})
}

fn result(id: &str, origin: Value, station: &str, evidence: Value) -> CommandResult {
    let mut result = fixtures::result(id, origin, station, fixtures::variables());
    result.device_model_201 = None;
    result.schema_version = ContractVersion::V1_CHARGING_PROFILE_201;
    let evidence: ChargingProfileResult201 = serde_json::from_value(evidence).unwrap();
    let accepted = evidence.accepted();
    result.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted,
        error: (!accepted).then_some(uob_contracts::CommandError {
            code: uob_contracts::CommandErrorCode::ProtocolRejected,
            detail: None,
        }),
    };
    result.charging_profile_201 = Some(evidence);
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
async fn served_current_result_and_nested_schemas_preserve_native_status_exact_rates_and_omissions()
{
    let mut rejected = evidence(2);
    rejected["status"] = json!("Rejected");
    rejected["reason_code"] = json!("InvalidValue");
    let clear = json!({"action":"ClearChargingProfile","request":{"charging_profile_criteria":{
        "evse_id":2,"charging_profile_purpose":"TxDefaultProfile","stack_level":2
    }},"status":"Unknown","reason_code":"NotFound"});
    let accepted_clear = json!({"action":"ClearChargingProfile","request":{"charging_profile_id":-118},"status":"Accepted"});
    let mut uncertain = own("uncertain", evidence(2));
    uncertain.charging_profile_201 = None;
    uncertain.lifecycle = CommandLifecycle::TransmissionUncertain {
        detail: "protocol.response_timeout".to_owned(),
    };
    let mut host = Host::start(
        vec![
            own("set", evidence(1024)),
            own("rejected", rejected),
            own("unknown", clear),
            own("clear", accepted_clear),
            uncertain,
        ],
        true,
    )
    .await;
    let schemas = schemas::Schemas::fetch(&host).await;
    for (id, accepted, native_status) in [
        ("set", true, Some("Accepted")),
        ("rejected", false, Some("Rejected")),
        ("unknown", false, Some("Unknown")),
        ("clear", true, Some("Accepted")),
        ("uncertain", false, None),
    ] {
        assert_served_result(&host, &schemas, id, accepted, native_status).await;
    }
    let (_, body) = host.get("/bridge/v1/commands/set", OPERATOR).await;
    assert_eq!(body["charging_profile_201"], evidence(1024));
    let (status, historical) = host
        .get("/bridge/v1/schemas/v1.6/command-result.schema.json", READER)
        .await;
    assert_eq!(status, 200);
    let historical = jsonschema::draft202012::new(&historical).unwrap();
    for invalid in [
        json!({"action":"SetChargingProfile","request":{},"status":"Unknown"}),
        json!({"action":"ClearChargingProfile","request":{},"status":"NotSupported"}),
        json!({"action":"NotAnAction"}),
    ] {
        let mut invalid_result = body.clone();
        invalid_result["charging_profile_201"] = invalid;
        assert!(!schemas.status().is_valid(&invalid_result));
        assert!(!schemas.canonical(schemas::RESULT).is_valid(&invalid_result));
        assert!(
            !schemas
                .canonical(schemas::RECORD)
                .is_valid(&fixtures::export_record(&invalid_result))
        );
        assert!(
            !schemas
                .canonical(schemas::BATCH)
                .is_valid(&fixtures::export_batch(&invalid_result))
        );
        assert!(historical.is_valid(&invalid_result));
    }
    host.stop().await;
}

async fn assert_served_result(
    host: &Host,
    schemas: &schemas::Schemas,
    id: &str,
    accepted: bool,
    native_status: Option<&str>,
) {
    let (status, body) = host
        .get(&format!("/bridge/v1/commands/{id}"), OPERATOR)
        .await;
    assert_eq!(status, 200);
    schemas.canonical(schemas::RESULT).validate(&body).unwrap();
    schemas.status().validate(&body).unwrap();
    schemas
        .admitted()
        .validate(&json!({
            "request_id":id,"status_url":format!("/bridge/v1/commands/{id}"),"result":body
        }))
        .unwrap();
    schemas
        .canonical(schemas::RECORD)
        .validate(&fixtures::export_record(&body))
        .unwrap();
    schemas
        .canonical(schemas::BATCH)
        .validate(&fixtures::export_batch(&body))
        .unwrap();
    assert_eq!(body["schema_version"], json!({"major":1,"revision":7}));
    if let Some(native_status) = native_status {
        assert_eq!(body["lifecycle"]["accepted"], accepted);
        assert_eq!(body["charging_profile_201"]["status"], native_status);
    } else {
        assert!(body.get("charging_profile_201").is_none());
        assert_eq!(body["lifecycle"]["stage"], "transmission_uncertain");
    }
    assert!(body.get("charging_profile_16").is_none());
    assert!(body.get("observed_effects").is_none());
}

#[tokio::test]
async fn native_result_reads_require_exact_origin_principal_resource_and_host_grant() {
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
            .map(|(id, origin, station)| result(id, origin, station, evidence(2)))
            .collect(),
        true,
    )
    .await;
    assert_eq!(host.get("/bridge/v1/commands/own", OPERATOR).await.0, 200);
    assert_eq!(host.get("/bridge/v1/commands/own", READER).await.0, 403);
    assert_eq!(host.get("/bridge/v1/commands/own", "wrong").await.0, 401);
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
    ] {
        assert_eq!(
            host.get(&format!("/bridge/v1/commands/{id}"), credential)
                .await
                .0,
            404
        );
    }
    assert_native_ingress_denied(&mut host).await;
    host.stop().await;
    let mut denied = Host::start(vec![own("own", evidence(2))], false).await;
    // Missing host CommandStatus capability is Unsupported before querying any result,
    // unlike granted reads denied by origin/resource ownership, which remain 404 above.
    for id in ["own", "missing"] {
        assert_eq!(
            denied
                .get(&format!("/bridge/v1/commands/{id}"), OPERATOR)
                .await,
            (
                501,
                json!({"error":"ems_scada_http.operation_not_supported"})
            )
        );
    }
    denied.stop().await;
}

async fn assert_native_ingress_denied(host: &mut Host) {
    for action in ["SetChargingProfile", "ClearChargingProfile"] {
        let response = host
            .client
            .post(format!("{}/bridge/v1/commands", host.base))
            .bearer_auth(OPERATOR)
            .json(&json!({
                "request_id":format!("denied-{action}"),
                "resource":{"bridge_id":"site-01","station_id":"station-a"},
                "operation":{"kind":"ocpp","parameters":{
                    "protocol":"ocpp201","action":action,
                    "payload_schema":format!("urn:OCPP:Cp:2:2020:3:{action}Request"),"payload":{}
                }},
                "expires_at":"2099-01-01T00:00:00Z"
            }))
            .send()
            .await
            .unwrap();
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
}

#[tokio::test]
async fn full_maximum_period_evidence_keeps_existing_response_bound_and_no_truncated_fallback() {
    let mut large = own("large", evidence(1024));
    large.observed_effects = (0..2200).map(|index| serde_json::from_value(json!({
        "event_id":format!("independent-status-observation-{index}"),"event_type":"resource.status_changed",
        "observed_at":"2026-09-01T12:00:00Z"
    })).unwrap()).collect();
    assert!(serde_json::to_vec(&large).unwrap().len() > 256 * 1024);
    let mut host = Host::start(vec![large, own("full", evidence(1024))], true).await;
    assert_eq!(
        host.get("/bridge/v1/commands/large", OPERATOR).await,
        (413, json!({"error":"ems_scada_http.payload_too_large"}))
    );
    let (status, body) = host.get("/bridge/v1/commands/full", OPERATOR).await;
    assert_eq!(status, 200);
    assert_eq!(body["charging_profile_201"], evidence(1024));
    host.stop().await;
}
