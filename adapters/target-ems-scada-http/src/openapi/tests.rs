use super::{openapi_document, schemas::CANONICAL};
use crate::test_support::{READER_TOKEN, authenticated_router, get};
use serde_json::{Value, json};

#[path = "../../../../tests/ems-http-contract-client/probe.rs"]
mod probe;

fn registry() -> jsonschema::Registry<'static> {
    let resources = CANONICAL.iter().map(|(file, source)| {
        (
            format!("https://bridge.test/bridge/v1/schemas/v1.0/{file}"),
            serde_json::from_str::<Value>(source).unwrap(),
        )
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
fn published_contract_matches_routes_models_and_parameters() {
    let published: Value = serde_json::from_str(include_str!("../../openapi/v1.json")).unwrap();
    assert_eq!(
        published,
        openapi_document(),
        "regenerate using the export_openapi example"
    );
    let source = include_str!("../routing.rs");
    let actual: std::collections::BTreeSet<_> = source
        .split(".route(")
        .skip(1)
        .filter_map(|rest| {
            rest.trim_start()
                .strip_prefix('"')
                .and_then(|r| r.split('"').next())
        })
        .collect();
    let documented: std::collections::BTreeSet<_> = published["paths"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(actual, documented, "router drift");
    // Demonstrate the snapshot gate catches both route removal and a response model edit.
    for pointer in [
        "/paths/~1bridge~1v1~1events",
        "/components/schemas/PointView/properties/value",
    ] {
        let mut stale = published.clone();
        *stale.pointer_mut(pointer).unwrap() = Value::Null;
        assert_ne!(stale, openapi_document());
    }
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
async fn document_and_exact_canonical_files_share_authentication_and_bounds() {
    let router = authenticated_router();
    for path in [
        "/bridge/v1/openapi.json",
        "/bridge/v1/schemas/v1.0/station-snapshot.schema.json",
    ] {
        assert_eq!(get(router.clone(), path, None).await.0, 401);
        assert_eq!(get(router.clone(), path, Some("wrong")).await.0, 401);
    }
    let (status, document) = get(
        router.clone(),
        "/bridge/v1/openapi.json",
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(document, openapi_document());
    for (file, source) in CANONICAL {
        let (status, body) = get(
            router.clone(),
            &format!("/bridge/v1/schemas/v1.0/{file}"),
            Some(READER_TOKEN),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body, serde_json::from_str::<Value>(source).unwrap());
    }
    assert_eq!(
        get(
            router,
            "/bridge/v1/schemas/v1.0/unknown",
            Some(READER_TOKEN)
        )
        .await
        .0,
        404
    );
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
