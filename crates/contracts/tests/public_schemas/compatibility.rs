use super::*;

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
    let event = include_str!("../fixtures/event-envelope-v1.json").replace(
        "\n  \"payload\"",
        "\n  \"future_optional_event_field\": {\"enabled\": true},\n  \"payload\"",
    );
    serde_json::from_str::<EventEnvelope<Value>>(&event)
        .expect("older event reader accepts optional field");

    let mut result: Value =
        serde_json::from_str(include_str!("../fixtures/command-results-v1.json"))
            .expect("result fixture");
    result[0]["future_optional_result_field"] = json!("new metadata");
    serde_json::from_value::<CommandResult>(result[0].clone())
        .expect("older result reader accepts optional field");
}

#[test]
fn remote_correlation_is_an_additive_revision_of_released_schemas() {
    for (name, previous) in [
        (
            "station-snapshot",
            include_str!("../../schemas/v1.0/station-snapshot.schema.json"),
        ),
        (
            "export-record",
            include_str!("../../schemas/v1.0/export-record.schema.json"),
        ),
        (
            "export-record",
            include_str!("../../schemas/v1.1/export-record.schema.json"),
        ),
        (
            "export-batch",
            include_str!("../../schemas/v1.0/export-batch.schema.json"),
        ),
        (
            "export-batch",
            include_str!("../../schemas/v1.1/export-batch.schema.json"),
        ),
    ] {
        let old: Value = serde_json::from_str(previous).unwrap();
        let new = published(name);
        assert!(compatibility_errors(&old, &new, "$").is_empty());
        for (definition, previous) in old["$defs"].as_object().unwrap() {
            assert!(
                compatibility_errors(previous, &new["$defs"][definition], definition).is_empty()
            );
        }
        let state = &new["$defs"]["TransactionProtocolState"];
        assert!(state["properties"].get("remote_start_id").is_some());
        assert!(!strings(state.get("required")).contains("remote_start_id"));
    }
}

#[test]
fn configuration_result_adds_only_optional_fields_to_v1() {
    let old: Value = serde_json::from_str(include_str!(
        "../../schemas/v1.0/command-result.schema.json"
    ))
    .unwrap();
    let new = published("command-result");
    assert!(compatibility_errors(&old, &new, "$").is_empty());
    for (definition, previous) in old["$defs"].as_object().unwrap() {
        assert!(compatibility_errors(previous, &new["$defs"][definition], definition).is_empty());
    }
    for name in ["configuration", "configuration_observations"] {
        assert!(new["properties"].get(name).is_some());
        assert!(!strings(new.get("required")).contains(name));
    }
}
