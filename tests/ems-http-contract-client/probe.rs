//! Simulation-owned HTTP contract probe. No bridge models, handlers, or persistence imports.
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

type Failure = Box<dyn std::error::Error + Send + Sync>;

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
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let base = reqwest::Url::parse(base)?;
    if !matches!(base.scheme(), "http" | "https")
        || !base.username().is_empty()
        || base.password().is_some()
    {
        return Err("invalid API base".into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let (_, document) = fetch(&client, base.join("/bridge/v1/openapi.json")?, token).await?;
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
    for file in files {
        let file = file.as_str().ok_or("schema filename")?;
        if file.contains('/') || file.contains("..") {
            return Err("unsafe schema filename".into());
        }
        let url = base.join(&format!("/bridge/v1/schemas/v1.0/{file}"))?;
        let (_, schema) = fetch(&client, url.clone(), token).await?;
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
            let (status, body) = fetch(&client, base.join(&call.path)?, token).await?;
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
) -> Result<(u16, Value), Failure> {
    let mut response = client.get(url).bearer_auth(token).send().await?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}").into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err("response exceeds 1 MiB".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((status, serde_json::from_slice(&bytes)?))
}
