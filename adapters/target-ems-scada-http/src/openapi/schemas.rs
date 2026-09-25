use schemars::{JsonSchema, generate::SchemaSettings};
use serde_json::{Map, Value, json};

macro_rules! canonical {
    ($($name:literal),+ $(,)?) => {
        pub(super) const CANONICAL: &[(&str, &str)] = &[$((concat!($name, ".schema.json"),
            include_str!(concat!("../../../../crates/contracts/schemas/v1.0/", $name, ".schema.json")))),+];
    };
}
canonical!(
    "station-snapshot",
    "resource-ref",
    "resource-capabilities",
    "runtime-identity",
    "service-identity",
    "data-point-descriptor",
    "data-point-value",
    "command",
    "command-result",
    "event-envelope",
    "trace-record",
    "export-record",
    "export-batch",
    "export-report"
);

/// Only the changed result is published at v1.1; all v1.0 files remain byte-for-byte intact.
pub(super) const CANONICAL_V1_1: &[(&str, &str)] = &[(
    "command-result.schema.json",
    include_str!("../../../../crates/contracts/schemas/v1.1/command-result.schema.json"),
)];

pub(super) fn reference(name: &str) -> Value {
    let revision = if name == "command-result" {
        "v1.1"
    } else {
        "v1.0"
    };
    json!({"$ref": format!("/bridge/v1/schemas/{revision}/{name}.schema.json")})
}

pub(super) fn canonical(revision: &str) -> &'static [(&'static str, &'static str)] {
    match revision {
        "v1.0" => CANONICAL,
        "v1.1" => CANONICAL_V1_1,
        _ => &[],
    }
}

/// Canonical definitions always resolve to the exact versioned files served by this listener.
/// Only HTTP wrappers are generated here; domain definitions are never forked into `OpenAPI`.
pub(super) fn add<T: JsonSchema>(components: &mut Map<String, Value>, name: &str, serialize: bool) {
    let settings = if serialize {
        SchemaSettings::draft2020_12().for_serialize()
    } else {
        SchemaSettings::draft2020_12()
    };
    let mut root = serde_json::to_value(settings.into_generator().into_root_schema_for::<T>())
        .expect("schema serialization");
    let definitions = root
        .as_object_mut()
        .unwrap()
        .remove("$defs")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    root.as_object_mut().unwrap().remove("$schema");
    let mut external = Map::new();
    for (file, source) in CANONICAL {
        let schema: Value = serde_json::from_str(source).expect("canonical schema");
        let base = format!("/bridge/v1/schemas/v1.0/{file}");
        external
            .entry(schema["title"].as_str().unwrap().to_owned())
            .or_insert(json!(base));
        if let Some(defs) = schema["$defs"].as_object() {
            for key in defs.keys() {
                external
                    .entry(key.clone())
                    .or_insert(json!(format!("{base}#/$defs/{key}")));
            }
        }
    }
    for (file, source) in CANONICAL_V1_1 {
        let schema: Value = serde_json::from_str(source).expect("canonical schema");
        let base = format!("/bridge/v1/schemas/v1.1/{file}");
        external.insert(schema["title"].as_str().unwrap().to_owned(), json!(base));
        if let Some(defs) = schema["$defs"].as_object() {
            for key in defs.keys() {
                external
                    .entry(key.clone())
                    .or_insert_with(|| json!(format!("{base}#/$defs/{key}")));
            }
        }
    }
    rewrite(&mut root, &external);
    components.insert(name.to_owned(), root);
    for (name, mut schema) in definitions {
        if !external.contains_key(&name) {
            rewrite(&mut schema, &external);
            if let Some(previous) = components.insert(name.clone(), schema.clone()) {
                assert_eq!(previous, schema, "conflicting HTTP schema {name}");
            }
        }
    }
}

fn rewrite(value: &mut Value, external: &Map<String, Value>) {
    match value {
        Value::Object(object) => {
            if let Some(name) = object
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|s| s.strip_prefix("#/$defs/"))
            {
                object.insert(
                    "$ref".to_owned(),
                    external
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| json!(format!("#/components/schemas/{name}"))),
                );
            }
            for value in object.values_mut() {
                rewrite(value, external);
            }
        }
        Value::Array(values) => {
            for value in values {
                rewrite(value, external);
            }
        }
        _ => {}
    }
}
