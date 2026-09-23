//! Simulation-owned HTTP contract probe. No bridge models, handlers, or persistence imports.
use serde::Deserialize;
use serde_json::{Value, json};
pub mod exercise;
pub mod http;
pub mod sse;

type Failure = Box<dyn std::error::Error + Send + Sync>;

const MAX_SCHEMAS: usize = 32;
const MAX_SCHEMA_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Demo {
    pub scenario: Vec<Scenario>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub protocol: String,
    pub calls: Vec<Call>,
    #[serde(default)]
    pub command_station: Option<String>,
    #[serde(default)]
    pub reader_out_of_scope: bool,
    #[serde(default)]
    pub authorization_reference: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub operation: String,
    pub path: String,
}

/// Builds calls from a declarative scenario and validates real HTTP responses using the
/// fetched `OpenAPI` schemas. Redirects and arbitrary schema hosts are never followed.
pub async fn run(base: &str, token: &str, demo: &Demo) -> Result<usize, Failure> {
    if demo.scenario.is_empty() {
        return Err("at least one read-only scenario required".into());
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(45),
        run_with_deadline(base, token, demo),
    )
    .await
    .map_err(|_| "contract probe deadline exceeded")?
}

async fn run_with_deadline(base: &str, token: &str, demo: &Demo) -> Result<usize, Failure> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let http = http::Http::new(base)?;
    let base = &http.base;
    let client = &http.client;
    let (_, document, _) = fetch(
        client,
        http.url("/bridge/v1/openapi.json")?,
        token,
        MAX_RESPONSE_BYTES,
    )
    .await?;
    let mut registry = jsonschema::Registry::new();
    let paths = document["paths"].as_object().ok_or("missing paths")?;
    let schema_operation = &paths["/bridge/v1/schemas/v1.0/{schema}"]["get"];
    let files = schema_operation["parameters"]
        .as_array()
        .ok_or("schema parameters")?
        .iter()
        .find(|p| p["name"] == "schema")
        .ok_or("schema path parameter")?["schema"]["enum"]
        .as_array()
        .ok_or("schema inventory")?;
    check_schema_inventory(files)?;
    let mut schema_bytes = 0;
    for file in files {
        let file = file.as_str().ok_or("schema filename")?;
        if file.contains('/') || file.contains("..") {
            return Err("unsafe schema filename".into());
        }
        let url = http.url(&format!("/bridge/v1/schemas/v1.0/{file}"))?;
        let (_, schema, bytes) = fetch(
            client,
            url.clone(),
            token,
            MAX_RESPONSE_BYTES.min(MAX_SCHEMA_BYTES - schema_bytes),
        )
        .await?;
        schema_bytes += bytes;
        registry = registry.add(url.as_str(), schema)?;
    }
    let registry = registry.prepare()?;
    let mut count = 0;
    for scenario in &demo.scenario {
        if !matches!(scenario.protocol.as_str(), "ocpp16" | "ocpp201") {
            return Err("unknown scenario protocol".into());
        }
        let mut saw_protocol = false;
        for call in &scenario.calls {
            let (template, operation) = paths
                .iter()
                .find_map(|(path, item)| {
                    (item["get"]["operationId"] == call.operation).then_some((path, &item["get"]))
                })
                .ok_or("unknown GET operation")?;
            if !matches_path(template, &call.path) {
                return Err("path does not match operation".into());
            }
            let (status, body, _) =
                fetch(client, http.url(&call.path)?, token, MAX_RESPONSE_BYTES).await?;
            let response = &operation["responses"][status.to_string()]["content"]["application/json"]
                ["schema"];
            if response.is_null() {
                return Err("undocumented HTTP response".into());
            }
            let root = json!({"$schema":"https://json-schema.org/draft/2020-12/schema",
                "components":document["components"],"allOf":[response]});
            let validator = jsonschema::options()
                .offline()
                .with_registry(&registry)
                .with_base_uri(base.join("/bridge/v1/openapi.json")?.as_str())
                .build(&root)?;
            validator.validate(&body).map_err(|e| e.to_string())?;
            if call.operation == "station" {
                let resources = body["resources"]
                    .as_array()
                    .ok_or("missing station resources")?;
                saw_protocol |= resources.iter().any(|r| {
                    r["resource"]["native_protocol_reference"]["protocol"] == scenario.protocol
                });
            }
            count += 1;
        }
        if !saw_protocol {
            return Err("scenario did not expose the expected native OCPP resource model".into());
        }
    }
    Ok(count)
}

fn check_schema_inventory(files: &[Value]) -> Result<(), Failure> {
    if files.len() > MAX_SCHEMAS {
        return Err("schema inventory exceeds bound".into());
    }
    Ok(())
}

fn matches_path(template: &str, path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    let expected: Vec<_> = template.split('/').collect();
    let actual: Vec<_> = path.split('/').collect();
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(a, b)| {
            if a.starts_with('{') {
                !b.is_empty() && b != "." && b != ".."
            } else {
                *a == b
            }
        })
}

async fn fetch(
    client: &reqwest::Client,
    url: reqwest::Url,
    token: &str,
    limit: usize,
) -> Result<(u16, Value, usize), Failure> {
    let mut response = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "HTTP request failed")?;
    let status = response.status().as_u16();
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("response exceeds bound".into());
    }
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}").into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "HTTP body failed")? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err("response exceeds bound".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let size = bytes.len();
    Ok((
        status,
        serde_json::from_slice(&bytes).map_err(|_| "invalid JSON response")?,
        size,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_scenario_fails_before_any_http_request() {
        use std::{io::ErrorKind, net::TcpListener, time::Duration};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let demo = Demo {
            scenario: Vec::new(),
        };

        let error = tokio::time::timeout(Duration::from_millis(500), run(&base, "reader", &demo))
            .await
            .expect("empty scenario must fail promptly")
            .expect_err("empty scenario must fail");
        assert_eq!(
            error.to_string(),
            "at least one read-only scenario required"
        );
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }

    #[test]
    fn inventory_limit_rejects_fanout_before_any_schema_request() {
        assert!(check_schema_inventory(&vec![Value::Null; MAX_SCHEMAS]).is_ok());
        assert!(check_schema_inventory(&vec![Value::Null; MAX_SCHEMAS + 1]).is_err());
    }
}
