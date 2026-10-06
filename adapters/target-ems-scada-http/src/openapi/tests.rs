use super::{
    openapi_document,
    schemas::{
        CANONICAL, CANONICAL_V1_1, CANONICAL_V1_2, CANONICAL_V1_3, CANONICAL_V1_4, CANONICAL_V1_5,
        CANONICAL_V1_6, CANONICAL_V1_7, CANONICAL_V1_8, CANONICAL_V1_9, CANONICAL_V1_10,
        CANONICAL_V1_11, CANONICAL_V1_12,
    },
};
use crate::test_support::{READER_TOKEN, authenticated_router, get};
use serde_json::{Value, json};

// This contract-only lib test includes the standalone client, including exercise-only paths.
#[allow(dead_code)]
#[path = "../../../../tests/ems-http-contract-client/probe.rs"]
mod probe;

fn registry() -> jsonschema::Registry<'static> {
    let resources = [
        ("v1.0", CANONICAL),
        ("v1.1", CANONICAL_V1_1),
        ("v1.2", CANONICAL_V1_2),
        ("v1.3", CANONICAL_V1_3),
        ("v1.4", CANONICAL_V1_4),
        ("v1.5", CANONICAL_V1_5),
        ("v1.6", CANONICAL_V1_6),
        ("v1.7", CANONICAL_V1_7),
        ("v1.8", CANONICAL_V1_8),
        ("v1.9", CANONICAL_V1_9),
        ("v1.10", CANONICAL_V1_10),
        ("v1.11", CANONICAL_V1_11),
        ("v1.12", CANONICAL_V1_12),
    ]
    .into_iter()
    .flat_map(|(revision, schemas)| {
        schemas.iter().map(move |(file, source)| {
            (
                format!("https://bridge.test/bridge/v1/schemas/{revision}/{file}"),
                serde_json::from_str::<Value>(source).unwrap(),
            )
        })
    });
    jsonschema::Registry::new()
        .extend(resources)
        .unwrap()
        .prepare()
        .unwrap()
}

fn validator(document: &Value, schema: &Value) -> jsonschema::Validator {
    let root = json!({"$schema":"https://json-schema.org/draft/2020-12/schema",
        "components":document["components"],"allOf":[schema]});
    jsonschema::options()
        .offline()
        .with_registry(&registry())
        .with_base_uri("https://bridge.test/bridge/v1/openapi.json")
        .build(&root)
        .unwrap()
}

#[test]
fn official_openapi_validation_and_every_schema_reference_pass_offline() {
    let document = openapi_document();
    let meta: Value =
        serde_json::from_str(include_str!("../../openapi/oas-3.1-schema-2025-09-15.json")).unwrap();
    jsonschema::draft202012::new(&meta)
        .unwrap()
        .validate(&document)
        .unwrap();
    for name in document["components"]["schemas"]
        .as_object()
        .unwrap()
        .keys()
    {
        validator(
            &document,
            &json!({"$ref":format!("#/components/schemas/{name}")}),
        );
    }
    for path in document["paths"].as_object().unwrap().values() {
        for operation in path.as_object().unwrap().values() {
            for response in operation["responses"].as_object().unwrap().values() {
                if let Some(content) = response["content"].as_object() {
                    for media in content.values() {
                        validator(&document, &media["schema"]);
                    }
                }
            }
        }
    }
    let request = &document["paths"]["/bridge/v1/commands"]["post"]["requestBody"]["content"]["application/json"];
    let validator = validator(&document, &request["schema"]);
    let example = &request["examples"]["start"]["value"];
    validator.validate(example).unwrap();
    let mut invalid = example.clone();
    invalid["origin"] = json!({"kind":"bridge"});
    assert!(!validator.is_valid(&invalid));
    invalid = example.clone();
    invalid["operation"]["kind"] = json!("invented");
    assert!(!validator.is_valid(&invalid));
}

#[tokio::test]
async fn document_shares_authentication_and_serves_published_contract() {
    let router = authenticated_router();
    let path = "/bridge/v1/openapi.json";
    assert_eq!(get(router.clone(), path, None).await.0, 401);
    assert_eq!(get(router.clone(), path, Some("wrong")).await.0, 401);
    let (status, document) = get(
        router.clone(),
        "/bridge/v1/openapi.json",
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(document, openapi_document());
}

#[tokio::test]
async fn schema_versions_serve_exact_canonical_files() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, header},
    };
    use tower::ServiceExt as _;

    let router = authenticated_router();
    for (revision, schemas) in [
        ("v1.0", CANONICAL),
        ("v1.1", CANONICAL_V1_1),
        ("v1.2", CANONICAL_V1_2),
        ("v1.3", CANONICAL_V1_3),
        ("v1.4", CANONICAL_V1_4),
        ("v1.5", CANONICAL_V1_5),
        ("v1.6", CANONICAL_V1_6),
        ("v1.7", CANONICAL_V1_7),
        ("v1.8", CANONICAL_V1_8),
        ("v1.9", CANONICAL_V1_9),
        ("v1.10", CANONICAL_V1_10),
        ("v1.11", CANONICAL_V1_11),
        ("v1.12", CANONICAL_V1_12),
    ] {
        for (file, source) in schemas {
            let path = format!("/bridge/v1/schemas/{revision}/{file}");
            for credential in [None, Some("wrong")] {
                assert_eq!(get(router.clone(), &path, credential).await.0, 401);
            }
            let request = Request::builder()
                .uri(&path)
                .header(header::AUTHORIZATION, format!("Bearer {READER_TOKEN}"))
                .body(Body::empty())
                .unwrap();
            let response = router.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), 200, "{path}");
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "application/schema+json"
            );
            let body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
            assert_eq!(body.as_ref(), source.as_bytes(), "{path}");
        }
        let path = format!("/bridge/v1/schemas/{revision}/unknown.schema.json");
        assert_eq!(get(router.clone(), &path, None).await.0, 401);
        let (status, body) = get(router.clone(), &path, Some(READER_TOKEN)).await;
        assert_eq!(status, 404);
        assert_eq!(body["error"], "ems_scada_http.unknown_resource");
    }
    for path in ["/bridge/v1/schemas/v1.1/station-snapshot.schema.json"] {
        assert_eq!(get(router.clone(), path, Some(READER_TOKEN)).await.0, 404);
    }
}

#[tokio::test]
async fn broker_free_contract_demo_validates_both_ocpp_resource_scenarios() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, authenticated_router()).await.unwrap();
    });
    let demo: probe::Demo = toml::from_str(include_str!(
        "../../../../tests/ems-http-contract-client/demo.toml"
    ))
    .unwrap();
    let result = probe::run(&format!("http://{address}"), READER_TOKEN, &demo).await;
    server.abort();
    assert_eq!(result.unwrap(), 8);
}

/// Shared by existing route scenarios so contract validation observes actual handlers/results.
pub(crate) fn assert_response(method: &str, path: &str, status: u16, body: &Value) {
    let document = openapi_document();
    let actual: Vec<_> = path.split('?').next().unwrap().split('/').collect();
    let template = document["paths"]
        .as_object()
        .unwrap()
        .keys()
        .find(|candidate| {
            let parts: Vec<_> = candidate.split('/').collect();
            parts.len() == actual.len()
                && parts
                    .iter()
                    .zip(&actual)
                    .all(|(a, b)| a.starts_with('{') || a == b)
        });
    let Some(template) = template else {
        return;
    };
    let operation = &document["paths"][template][method.to_ascii_lowercase()];
    if operation.is_null() {
        return;
    } // the common 405 fallback is tested separately
    let schema =
        &operation["responses"][status.to_string()]["content"]["application/json"]["schema"];
    if template.contains("schemas/") {
        return;
    } // application/schema+json, checked byte-for-byte
    assert!(
        !schema.is_null(),
        "undocumented {method} {template} status {status}"
    );
    validator(&document, schema)
        .validate(body)
        .unwrap_or_else(|e| panic!("{method} {template} {status}: {e}"));
}

pub(crate) fn assert_sse_payload(event: &str, body: &Value) {
    let document = openapi_document();
    let schema =
        &document["paths"]["/bridge/v1/events"]["get"]["x-uob-sse-events"][event]["schema"];
    assert!(!schema.is_null(), "undocumented SSE event {event}");
    validator(&document, schema).validate(body).unwrap();
}

#[tokio::test]
async fn malformed_path_identifiers_use_the_documented_error() {
    for path in [
        "/bridge/v1/stations/%FF",
        "/bridge/v1/points/%FF?station_id=station-a",
    ] {
        let (status, body) = get(authenticated_router(), path, Some(READER_TOKEN)).await;
        assert_eq!(status, 400);
        assert_eq!(body["error"], "ems_scada_http.invalid_request");
    }
}
