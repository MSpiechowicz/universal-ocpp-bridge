use axum::http::StatusCode;

use crate::{
    configuration::IntegrationCredentials,
    test_support::{READER_TOKEN, authenticated_router, descriptor, get, router, send},
};

#[tokio::test]
async fn the_capability_response_advertises_exactly_the_routes_this_build_serves() {
    let router = router(IntegrationCredentials::default());
    let (status, body) = get(router.clone(), "/bridge/v1/capabilities", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, document) = get(router.clone(), "/bridge/v1/openapi.json", None).await;
    assert_eq!(status, StatusCode::OK);

    for resource in body["resources"].as_array().expect("resource list") {
        let template = resource["path"].as_str().expect("resource path");
        if template.contains("{schema}") {
            let parameter = document["paths"][template]["get"]["parameters"]
                .as_array()
                .expect("advertised schema path parameters")
                .iter()
                .find(|parameter| parameter["in"] == "path" && parameter["name"] == "schema")
                .expect("advertised schema path parameter");
            for schema in parameter["schema"]["enum"]
                .as_array()
                .expect("advertised schema filenames")
            {
                assert_advertised_resource_is_mounted(
                    router.clone(),
                    resource,
                    Some(schema.as_str().expect("schema filename")),
                )
                .await;
            }
        } else {
            assert_advertised_resource_is_mounted(router.clone(), resource, None).await;
        }
    }
}

async fn assert_advertised_resource_is_mounted(
    router: axum::Router,
    resource: &serde_json::Value,
    schema: Option<&str>,
) {
    let path = resource["path"]
        .as_str()
        .expect("resource path")
        .replace("{schema}", schema.unwrap_or_default())
        .replace("{request_id}", "request-a")
        .replace("{station_id}", "station-a")
        .replace("{point_id}", "energy.active.import.register");
    // An advertised resource must be mounted. Credential-based permission to read it
    // remains a separate question from the public resource contract.
    let method = if resource["operations"][0] == "control" && resource["name"] == "commands" {
        "POST"
    } else {
        "GET"
    };
    let (status, _) = send(router, method, &path, None).await;
    assert!(
        !matches!(
            status,
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ),
        "advertised resource {path} is not served"
    );
}

#[tokio::test]
async fn an_uncredentialed_listener_still_reads_no_canonical_state() {
    // The open loopback default lets a local operator read the contract description. It grants
    // no reader role and no resource scope, so no canonical record is reachable through it.
    for path in [
        "/bridge/v1/stations",
        "/bridge/v1/stations/station-a",
        "/bridge/v1/points",
        "/bridge/v1/points/energy.active.import.register?station_id=station-a",
    ] {
        let (status, body) = get(router(IntegrationCredentials::default()), path, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(body["error"], "ems_scada_http.permission_denied", "{path}");
    }
}

#[tokio::test]
async fn uncredentialed_loopback_exposes_current_and_historical_schemas() {
    for path in [
        "/bridge/v1/schemas/v1.0/command-result.schema.json",
        "/bridge/v1/schemas/v1.3/command-result.schema.json",
        "/bridge/v1/schemas/v1.4/command-result.schema.json",
        "/bridge/v1/schemas/v1.4/export-record.schema.json",
        "/bridge/v1/schemas/v1.5/export-record.schema.json",
        "/bridge/v1/schemas/v1.5/export-batch.schema.json",
    ] {
        let (status, body) = get(router(IntegrationCredentials::default()), path, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let file = path.rsplit('/').next().unwrap();
        let revision = path.split('/').nth(4).unwrap();
        assert_eq!(
            body["$id"],
            format!("https://schemas.universal-ocpp-bridge.dev/contracts/{revision}/{file}")
        );
    }
}

#[tokio::test]
async fn the_capability_response_matches_the_supervised_target_descriptor() {
    let descriptor = descriptor();
    let (_, body) = get(
        router(IntegrationCredentials::default()),
        "/bridge/v1/capabilities",
        None,
    )
    .await;

    assert_eq!(
        body["contract_version"]["major"],
        u64::from(descriptor.contract_version.major)
    );
    assert_eq!(body["target"]["kind"], descriptor.kind.as_str());
    assert_eq!(
        body["target"]["instance_id"],
        descriptor.instance_id.as_str()
    );
    assert_eq!(
        body["outbound_message_classes"],
        serde_json::json!(["station_snapshot", "domain_event", "command_result"])
    );
    assert_eq!(
        body["delivery_semantics"],
        serde_json::json!(["local_exposure"])
    );
    assert_eq!(descriptor.inbound_operations.len(), 3);
    assert_eq!(
        body["inbound_operations"],
        serde_json::to_value(&descriptor.inbound_operations).unwrap()
    );
    assert_eq!(
        body["limits"]["maximum_message_bytes"],
        descriptor.limits.maximum_message_bytes
    );
    assert_eq!(
        body["limits"]["maximum_in_flight_deliveries"],
        descriptor.limits.maximum_in_flight_deliveries
    );
    assert_eq!(
        body["limits"]["maximum_in_flight_commands"],
        descriptor.limits.maximum_in_flight_commands
    );
}

#[tokio::test]
async fn management_debug_and_simulator_paths_are_unreachable_with_a_stable_code() {
    for path in [
        "/api/v1/health",
        "/bridge/v1/stations/station-a/points",
        "/api/v1/identity",
        "/api/v1/commands",
        "/api/v1/events",
        "/api/v1/stations",
        "/metrics",
        "/health",
        "/",
        "/bridge/v1",
        "/bridge/v2/capabilities",
    ] {
        let (status, body) = get(router(IntegrationCredentials::default()), path, None).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{path} must not be served here"
        );
        assert_eq!(body["error"], "ems_scada_http.unknown_resource", "{path}");
    }
}

#[tokio::test]
async fn an_unsupported_method_is_reported_separately_from_an_unknown_resource() {
    let (status, body) = send(
        router(IntegrationCredentials::default()),
        "POST",
        "/bridge/v1/capabilities",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body["error"], "ems_scada_http.unsupported_operation");
}

#[tokio::test]
async fn a_credentialed_listener_authenticates_every_integration_request() {
    let (status, body) = get(authenticated_router(), "/bridge/v1/capabilities", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "ems_scada_http.unauthenticated");

    let (status, body) = get(
        authenticated_router(),
        "/bridge/v1/capabilities",
        Some("not-the-token"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "ems_scada_http.invalid_credential");
}

#[tokio::test]
async fn an_authenticated_caller_sees_only_its_own_scope() {
    let (status, body) = get(
        authenticated_router(),
        "/bridge/v1/capabilities",
        Some(READER_TOKEN),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["caller"]["permissions"], serde_json::json!(["read"]));
    assert_eq!(
        body["caller"]["resource_scopes"],
        serde_json::json!([{"scope": "bridge", "bridge_id": "site-01"}])
    );
}

#[tokio::test]
async fn an_unknown_resource_is_refused_before_credentials_are_considered() {
    let (status, body) = get(
        authenticated_router(),
        "/api/v1/identity",
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "ems_scada_http.unknown_resource");
}
