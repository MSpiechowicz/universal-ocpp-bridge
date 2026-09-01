use std::collections::BTreeSet;

use schemars::{JsonSchema, schema_for};
use serde_json::{Map, Value, json};
use uob_contracts::{
    Command, CommandResult, DataPointDescriptor, DataPointValue, EventEnvelope, ExportBatch,
    ExportRecord, ExportReport, ResourceCapabilities, ResourceRef, RuntimeIdentity,
    ServiceIdentity, StationSnapshot, TraceRecord,
};

const SCHEMAS: &[(&str, &str)] = &[
    (
        "station-snapshot",
        include_str!("../schemas/v1.0/station-snapshot.schema.json"),
    ),
    (
        "resource-ref",
        include_str!("../schemas/v1.0/resource-ref.schema.json"),
    ),
    (
        "resource-capabilities",
        include_str!("../schemas/v1.0/resource-capabilities.schema.json"),
    ),
    (
        "runtime-identity",
        include_str!("../schemas/v1.0/runtime-identity.schema.json"),
    ),
    (
        "service-identity",
        include_str!("../schemas/v1.0/service-identity.schema.json"),
    ),
    (
        "data-point-descriptor",
        include_str!("../schemas/v1.0/data-point-descriptor.schema.json"),
    ),
    (
        "data-point-value",
        include_str!("../schemas/v1.0/data-point-value.schema.json"),
    ),
    (
        "command",
        include_str!("../schemas/v1.0/command.schema.json"),
    ),
    (
        "command-result",
        include_str!("../schemas/v1.0/command-result.schema.json"),
    ),
    (
        "event-envelope",
        include_str!("../schemas/v1.0/event-envelope.schema.json"),
    ),
    (
        "trace-record",
        include_str!("../schemas/v1.0/trace-record.schema.json"),
    ),
    (
        "export-record",
        include_str!("../schemas/v1.0/export-record.schema.json"),
    ),
    (
        "export-batch",
        include_str!("../schemas/v1.0/export-batch.schema.json"),
    ),
    (
        "export-report",
        include_str!("../schemas/v1.0/export-report.schema.json"),
    ),
];

fn published(name: &str) -> Value {
    let (_, schema) = SCHEMAS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .expect("known public schema");
    serde_json::from_str(schema).expect("valid checked-in schema")
}

fn generated<T: JsonSchema>(name: &str) -> Value {
    let mut value = serde_json::to_value(schema_for!(T)).expect("serialize generated schema");
    let object = value.as_object_mut().expect("schema object");
    object.insert(
        "$id".to_owned(),
        Value::String(format!(
            "https://schemas.universal-ocpp-bridge.dev/contracts/v1.0/{name}.schema.json"
        )),
    );
    object.insert(
        "x-uob-contract-version".to_owned(),
        json!({ "major": 1, "revision": 0 }),
    );
    value
}

fn assert_valid(name: &str, instance: &Value) {
    let schema = published(name);
    let validator = jsonschema::draft202012::new(&schema).expect("compile Draft 2020-12 schema");
    let errors = validator
        .iter_errors(instance)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "{name} fixture did not validate:\n{}",
        errors.join("\n")
    );
}

#[test]
fn published_schema_snapshots_match_the_rust_contracts() {
    assert_eq!(
        published("station-snapshot"),
        generated::<StationSnapshot>("station-snapshot")
    );
    assert_eq!(
        published("resource-ref"),
        generated::<ResourceRef>("resource-ref")
    );
    assert_eq!(
        published("resource-capabilities"),
        generated::<ResourceCapabilities>("resource-capabilities")
    );
    assert_eq!(
        published("runtime-identity"),
        generated::<RuntimeIdentity>("runtime-identity")
    );
    assert_eq!(
        published("service-identity"),
        generated::<ServiceIdentity>("service-identity")
    );
    assert_eq!(
        published("data-point-descriptor"),
        generated::<DataPointDescriptor>("data-point-descriptor")
    );
    assert_eq!(
        published("data-point-value"),
        generated::<DataPointValue>("data-point-value")
    );
    assert_eq!(published("command"), generated::<Command<Value>>("command"));
    assert_eq!(
        published("command-result"),
        generated::<CommandResult>("command-result")
    );
    assert_eq!(
        published("event-envelope"),
        generated::<EventEnvelope<Value>>("event-envelope")
    );
    assert_eq!(
        published("trace-record"),
        generated::<TraceRecord>("trace-record")
    );
    assert_eq!(
        published("export-record"),
        generated::<ExportRecord>("export-record")
    );
    assert_eq!(
        published("export-batch"),
        generated::<ExportBatch>("export-batch")
    );
    assert_eq!(
        published("export-report"),
        generated::<ExportReport>("export-report")
    );
}

#[test]
fn canonical_examples_validate_against_their_public_schemas() {
    let ocpp16: Value =
        serde_json::from_str(include_str!("fixtures/station-snapshot-ocpp16-v1.json"))
            .expect("OCPP 1.6 snapshot fixture");
    let ocpp201: Value =
        serde_json::from_str(include_str!("fixtures/station-snapshot-ocpp201-v1.json"))
            .expect("OCPP 2.0.1 snapshot fixture");
    assert_valid("station-snapshot", &ocpp16);
    assert_valid("station-snapshot", &ocpp201);
    assert_valid("resource-ref", &ocpp16["station"]);
    assert_valid("resource-ref", &ocpp201["resources"][0]["resource"]);
    assert_valid("resource-capabilities", &ocpp201["capabilities"]);

    let event: Value = serde_json::from_str(include_str!("fixtures/event-envelope-v1.json"))
        .expect("event fixture");
    assert_valid("event-envelope", &event);
    assert_valid("runtime-identity", &event["runtime"]);
    assert_valid(
        "service-identity",
        &json!({
            "bridge_id": "bridge-berlin-1",
            "runtime": event["runtime"],
            "selected_target_id": "main-ems"
        }),
    );
    assert_valid(
        "data-point-descriptor",
        &serde_json::from_str(include_str!("fixtures/data-point-descriptor-v1.json"))
            .expect("descriptor fixture"),
    );
    assert_valid(
        "data-point-value",
        &serde_json::from_str(include_str!("fixtures/data-point-value-v1.json"))
            .expect("point fixture"),
    );
    assert_valid(
        "command",
        &serde_json::from_str(include_str!("fixtures/command-start-v1.json"))
            .expect("command fixture"),
    );
    let results: Value = serde_json::from_str(include_str!("fixtures/command-results-v1.json"))
        .expect("command result fixtures");
    for result in results.as_array().expect("result fixture array") {
        assert_valid("command-result", result);
    }
    assert_valid(
        "trace-record",
        &serde_json::from_str(include_str!("fixtures/trace-record-v1.json"))
            .expect("trace fixture"),
    );
    let export_batch: Value = serde_json::from_str(include_str!("fixtures/export-batch-v1.json"))
        .expect("export batch fixture");
    assert_valid("export-batch", &export_batch);
    for record in export_batch["records"]
        .as_array()
        .expect("export record fixture array")
    {
        assert_valid("export-record", record);
    }
}

#[test]
fn schemas_reject_precision_loss_collapsed_resources_and_unknown_commands() {
    let mut numeric_decimal: Value =
        serde_json::from_str(include_str!("fixtures/data-point-value-v1.json"))
            .expect("point fixture");
    numeric_decimal["value"]["value"] = json!(12.345_678_901_234_567_f64);
    let point_schema = published("data-point-value");
    let point_validator =
        jsonschema::draft202012::new(&point_schema).expect("compile point schema");
    assert!(!point_validator.is_valid(&numeric_decimal));
    numeric_decimal["value"]["value"] = json!("12.3 watts");
    assert!(!point_validator.is_valid(&numeric_decimal));

    let collapsed_resource = json!({
        "bridge_id": "bridge-berlin-1",
        "station_id": "station-201",
        "resource": { "kind": "evse", "connector_id": "1" }
    });
    let resource_schema = published("resource-ref");
    let resource_validator =
        jsonschema::draft202012::new(&resource_schema).expect("compile resource schema");
    assert!(!resource_validator.is_valid(&collapsed_resource));

    let mut unknown_command: Value =
        serde_json::from_str(include_str!("fixtures/command-start-v1.json"))
            .expect("command fixture");
    unknown_command["operation"] = json!({ "kind": "mirror_observed_state", "parameters": {} });
    let command_schema = published("command");
    let command_validator =
        jsonschema::draft202012::new(&command_schema).expect("compile command schema");
    assert!(!command_validator.is_valid(&unknown_command));
}

fn strings(value: Option<&Value>) -> BTreeSet<&str> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn compatibility_errors(old: &Value, new: &Value, path: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let (Some(old), Some(new)) = (old.as_object(), new.as_object()) else {
        if old != new {
            errors.push(format!("{path}: schema value changed"));
        }
        return errors;
    };

    for keyword in ["type", "const"] {
        if old.get(keyword).is_some() && old.get(keyword) != new.get(keyword) {
            errors.push(format!("{path}: {keyword} changed"));
        }
    }

    let old_enum = old.get("enum").and_then(Value::as_array);
    let new_enum = new.get("enum").and_then(Value::as_array);
    if let (Some(old_values), Some(new_values)) = (old_enum, new_enum) {
        for value in old_values {
            if !new_values.contains(value) {
                errors.push(format!("{path}: accepted enum value {value} was removed"));
            }
        }
    } else if old_enum.is_some() != new_enum.is_some() {
        errors.push(format!("{path}: enum semantics changed"));
    }

    let old_required = strings(old.get("required"));
    let new_required = strings(new.get("required"));
    for field in new_required.difference(&old_required) {
        errors.push(format!("{path}: optional field {field} became required"));
    }

    let empty = Map::new();
    let old_properties = old
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let new_properties = new
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    for (name, old_property) in old_properties {
        if let Some(new_property) = new_properties.get(name) {
            errors.extend(compatibility_errors(
                old_property,
                new_property,
                &format!("{path}/properties/{name}"),
            ));
        } else {
            errors.push(format!("{path}: property {name} was removed"));
        }
    }

    for keyword in ["items"] {
        if let (Some(old_child), Some(new_child)) = (old.get(keyword), new.get(keyword)) {
            errors.extend(compatibility_errors(
                old_child,
                new_child,
                &format!("{path}/{keyword}"),
            ));
        }
    }
    errors
}

#[test]
fn compatibility_check_detects_incompatible_v1_changes() {
    let baseline = json!({
        "type": "object",
        "properties": {
            "status": { "type": "string", "enum": ["ready", "degraded"] },
            "detail": { "type": "string" }
        },
        "required": ["status"]
    });
    let additive = json!({
        "type": "object",
        "properties": {
            "status": { "type": "string", "enum": ["ready", "degraded", "stopped"] },
            "detail": { "type": "string" },
            "optional_metric": { "type": "integer" }
        },
        "required": ["status"]
    });
    assert!(compatibility_errors(&baseline, &additive, "$").is_empty());

    let removal = json!({
        "type": "object",
        "properties": { "status": { "type": "string", "enum": ["ready", "degraded"] } },
        "required": ["status"]
    });
    assert!(
        compatibility_errors(&baseline, &removal, "$")
            .iter()
            .any(|error| error.contains("property detail was removed"))
    );

    let changed_type = json!({
        "type": "object",
        "properties": {
            "status": { "type": "integer", "enum": ["ready", "degraded"] },
            "detail": { "type": "string" }
        },
        "required": ["status"]
    });
    assert!(
        compatibility_errors(&baseline, &changed_type, "$")
            .iter()
            .any(|error| error.contains("type changed"))
    );

    let narrowed_enum = json!({
        "type": "object",
        "properties": {
            "status": { "type": "string", "enum": ["ready"] },
            "detail": { "type": "string" }
        },
        "required": ["status"]
    });
    assert!(
        compatibility_errors(&baseline, &narrowed_enum, "$")
            .iter()
            .any(|error| error.contains("accepted enum value"))
    );

    let newly_required = json!({
        "type": "object",
        "properties": {
            "status": { "type": "string", "enum": ["ready", "degraded"] },
            "detail": { "type": "string" }
        },
        "required": ["status", "detail"]
    });
    assert!(
        compatibility_errors(&baseline, &newly_required, "$")
            .iter()
            .any(|error| error.contains("became required"))
    );
}

#[test]
fn older_readers_tolerate_optional_response_and_event_fields() {
    let event = include_str!("fixtures/event-envelope-v1.json").replace(
        "\n  \"payload\"",
        "\n  \"future_optional_event_field\": {\"enabled\": true},\n  \"payload\"",
    );
    serde_json::from_str::<EventEnvelope<Value>>(&event)
        .expect("older event reader accepts optional field");

    let mut result: Value = serde_json::from_str(include_str!("fixtures/command-results-v1.json"))
        .expect("result fixture");
    result[0]["future_optional_result_field"] = json!("new metadata");
    serde_json::from_value::<CommandResult>(result[0].clone())
        .expect("older result reader accepts optional field");
}
