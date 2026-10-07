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
    "export-report",
    "set-variables-reference-201",
    "set-network-profile-reference-201",
    "send-local-list-reference-16",
    "send-local-list-reference-201",
    "reserve-now-reference-16",
    "reserve-now-reference-201"
);

/// Only the changed result is published at v1.1; all v1.0 files remain byte-for-byte intact.
pub(super) const CANONICAL_V1_1: &[(&str, &str)] = &[(
    "command-result.schema.json",
    include_str!("../../../../crates/contracts/schemas/v1.1/command-result.schema.json"),
)];

pub(super) const CANONICAL_V1_2: &[(&str, &str)] = &[(
    "command-result.schema.json",
    include_str!("../../../../crates/contracts/schemas/v1.2/command-result.schema.json"),
)];

pub(super) const CANONICAL_V1_3: &[(&str, &str)] = &[
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.3/command-result.schema.json"),
    ),
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.3/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.3/export-batch.schema.json"),
    ),
];

pub(super) const CANONICAL_V1_4: &[(&str, &str)] = &[
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.4/command-result.schema.json"),
    ),
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.4/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.4/export-batch.schema.json"),
    ),
];

pub(super) const CANONICAL_V1_5: &[(&str, &str)] = &[
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.5/command-result.schema.json"),
    ),
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.5/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.5/export-batch.schema.json"),
    ),
];

pub(super) const CANONICAL_V1_6: &[(&str, &str)] = &[
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.6/command-result.schema.json"),
    ),
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.6/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.6/export-batch.schema.json"),
    ),
];

pub(super) const CANONICAL_V1_7: &[(&str, &str)] = &[
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.7/command-result.schema.json"),
    ),
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.7/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.7/export-batch.schema.json"),
    ),
];

pub(super) const CANONICAL_V1_8: &[(&str, &str)] = &[
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.8/command-result.schema.json"),
    ),
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.8/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.8/export-batch.schema.json"),
    ),
];

pub(super) const CANONICAL_V1_9: &[(&str, &str)] = &[
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.9/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.9/export-batch.schema.json"),
    ),
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.9/command-result.schema.json"),
    ),
];

pub(super) const CANONICAL_V1_10: &[(&str, &str)] = &[
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.10/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.10/export-batch.schema.json"),
    ),
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.10/command-result.schema.json"),
    ),
];
pub(super) const CANONICAL_V1_11: &[(&str, &str)] = &[
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.11/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.11/export-batch.schema.json"),
    ),
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.11/command-result.schema.json"),
    ),
];
pub(super) const CANONICAL_V1_12: &[(&str, &str)] = &[
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.12/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.12/export-batch.schema.json"),
    ),
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.12/command-result.schema.json"),
    ),
];
pub(super) const CANONICAL_V1_13: &[(&str, &str)] = &[
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.13/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.13/export-batch.schema.json"),
    ),
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.13/command-result.schema.json"),
    ),
];
pub(super) const CANONICAL_V1_14: &[(&str, &str)] = &[
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.14/command-result.schema.json"),
    ),
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.14/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.14/export-batch.schema.json"),
    ),
];
pub(super) const CANONICAL_V1_15: &[(&str, &str)] = &[
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.15/command-result.schema.json"),
    ),
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.15/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.15/export-batch.schema.json"),
    ),
];
pub(super) const CANONICAL_V1_16: &[(&str, &str)] = &[
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.16/command-result.schema.json"),
    ),
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.16/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.16/export-batch.schema.json"),
    ),
];
pub(super) const CANONICAL_V1_17: &[(&str, &str)] = &[
    (
        "command-result.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.17/command-result.schema.json"),
    ),
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.17/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.17/export-batch.schema.json"),
    ),
];
pub(super) const CANONICAL_V1_18: &[(&str, &str)] = &[
    (
        "export-record.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.18/export-record.schema.json"),
    ),
    (
        "export-batch.schema.json",
        include_str!("../../../../crates/contracts/schemas/v1.18/export-batch.schema.json"),
    ),
];

pub(super) fn reference(name: &str) -> Value {
    let revision = match name {
        "command-result" => "v1.17",
        "export-record" | "export-batch" => "v1.18",
        _ => "v1.0",
    };
    json!({"$ref": format!("/bridge/v1/schemas/{revision}/{name}.schema.json")})
}

pub(super) fn canonical(revision: &str) -> &'static [(&'static str, &'static str)] {
    match revision {
        "v1.0" => CANONICAL,
        "v1.1" => CANONICAL_V1_1,
        "v1.2" => CANONICAL_V1_2,
        "v1.3" => CANONICAL_V1_3,
        "v1.4" => CANONICAL_V1_4,
        "v1.5" => CANONICAL_V1_5,
        "v1.6" => CANONICAL_V1_6,
        "v1.7" => CANONICAL_V1_7,
        "v1.8" => CANONICAL_V1_8,
        "v1.9" => CANONICAL_V1_9,
        "v1.10" => CANONICAL_V1_10,
        "v1.11" => CANONICAL_V1_11,
        "v1.12" => CANONICAL_V1_12,
        "v1.13" => CANONICAL_V1_13,
        "v1.14" => CANONICAL_V1_14,
        "v1.15" => CANONICAL_V1_15,
        "v1.16" => CANONICAL_V1_16,
        "v1.17" => CANONICAL_V1_17,
        "v1.18" => CANONICAL_V1_18,
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
    for (revision, schemas) in [
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
    ] {
        for (file, source) in schemas {
            let schema: Value = serde_json::from_str(source).expect("canonical schema");
            let base = format!("/bridge/v1/schemas/{revision}/{file}");
            external.insert(schema["title"].as_str().unwrap().to_owned(), json!(base));
            if let Some(defs) = schema["$defs"].as_object() {
                for key in defs.keys() {
                    external
                        .entry(key.clone())
                        .or_insert_with(|| json!(format!("{base}#/$defs/{key}")));
                }
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
