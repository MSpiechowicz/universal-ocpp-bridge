use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::host::{Host, READER};

const BASE: &str = "https://bridge.test";
pub const RESULT: &str = "/bridge/v1/schemas/v1.10/command-result.schema.json";
pub const HISTORICAL: &str = "/bridge/v1/schemas/v1.4/command-result.schema.json";
pub const RECORD: &str = "/bridge/v1/schemas/v1.11/export-record.schema.json";
pub const BATCH: &str = "/bridge/v1/schemas/v1.11/export-batch.schema.json";

pub struct Schemas {
    pub document: Value,
    registry: jsonschema::Registry<'static>,
}

fn references(value: &Value, paths: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str)
                && reference.starts_with("/bridge/v1/schemas/")
            {
                paths.insert(reference.split('#').next().unwrap().to_owned());
            }
            for child in object.values() {
                references(child, paths);
            }
        }
        Value::Array(array) => {
            for child in array {
                references(child, paths);
            }
        }
        _ => {}
    }
}

impl Schemas {
    /// All external schemas are fetched from the actual listener; validation never uses the net.
    pub async fn fetch(host: &Host) -> Self {
        let (status, document) = host.get("/bridge/v1/openapi.json", READER).await;
        assert_eq!(status, 200);
        let mut paths = BTreeSet::from([
            RESULT.to_owned(),
            HISTORICAL.to_owned(),
            RECORD.to_owned(),
            BATCH.to_owned(),
        ]);
        references(&document, &mut paths);
        let mut resources = BTreeMap::new();
        for path in paths {
            let response = host
                .client
                .get(format!("{}{path}", host.base))
                .bearer_auth(READER)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200, "missing served schema {path}");
            assert_eq!(
                response.headers()["content-type"],
                "application/schema+json"
            );
            resources.insert(
                format!("{BASE}{path}"),
                response.json::<Value>().await.unwrap(),
            );
        }
        let registry = jsonschema::Registry::new()
            .extend(resources)
            .unwrap()
            .prepare()
            .unwrap();
        Self { document, registry }
    }

    pub fn validator(&self, schema: &Value) -> jsonschema::Validator {
        assert!(schema.is_object(), "missing served response schema");
        let root = json!({
            "$schema":"https://json-schema.org/draft/2020-12/schema",
            "components":self.document["components"],
            "allOf":[schema]
        });
        jsonschema::options()
            .offline()
            .with_registry(&self.registry)
            .with_base_uri("https://bridge.test/bridge/v1/openapi.json")
            .build(&root)
            .unwrap()
    }

    pub fn canonical(&self, path: &str) -> jsonschema::Validator {
        self.validator(&json!({"$ref":path}))
    }

    pub fn status(&self) -> jsonschema::Validator {
        self.validator(
            &self.document["paths"]["/bridge/v1/commands/{request_id}"]["get"]["responses"]["200"]
                ["content"]["application/json"]["schema"],
        )
    }
}
