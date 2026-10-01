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
fn trigger_and_schedule_results_add_only_optional_fields_to_released_schemas() {
    for (name, previous) in [
        (
            "command-result",
            include_str!("../../schemas/v1.0/command-result.schema.json"),
        ),
        (
            "command-result",
            include_str!("../../schemas/v1.1/command-result.schema.json"),
        ),
        (
            "export-record",
            include_str!("../../schemas/v1.2/export-record.schema.json"),
        ),
        (
            "export-batch",
            include_str!("../../schemas/v1.2/export-batch.schema.json"),
        ),
        (
            "command-result",
            include_str!("../../schemas/v1.2/command-result.schema.json"),
        ),
        (
            "export-record",
            include_str!("../../schemas/v1.3/export-record.schema.json"),
        ),
        (
            "export-batch",
            include_str!("../../schemas/v1.3/export-batch.schema.json"),
        ),
        (
            "command-result",
            include_str!("../../schemas/v1.3/command-result.schema.json"),
        ),
        (
            "export-record",
            include_str!("../../schemas/v1.4/export-record.schema.json"),
        ),
        (
            "export-batch",
            include_str!("../../schemas/v1.4/export-batch.schema.json"),
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
        let result = if name == "command-result" {
            &new
        } else {
            &new["$defs"]["CommandResult"]
        };
        assert!(result["properties"].get("trigger_observation").is_some());
        assert!(!strings(result.get("required")).contains("trigger_observation"));
        assert!(
            result["properties"]
                .get("trigger_observation_201")
                .is_some()
        );
        assert!(!strings(result.get("required")).contains("trigger_observation_201"));
        assert!(result["properties"].get("composite_schedule_16").is_some());
        assert!(!strings(result.get("required")).contains("composite_schedule_16"));
    }
}

#[test]
fn serialized_composite_schedules_validate_with_exact_rates_and_optional_metadata() {
    let fixtures: Value =
        serde_json::from_str(include_str!("../fixtures/command-results-v1.json")).unwrap();
    let mut result = fixtures[1].clone();
    result["schema_version"]["revision"] = json!(4);
    result["composite_schedule_16"] = json!({
        "request": {"connector_id": 1, "duration": 300, "charging_rate_unit": "W"},
        "status": "Accepted",
        "connector_id": 1,
        "schedule_start": "2026-09-01T14:00:03Z",
        "charging_schedule": {
            "duration": 300,
            "start_schedule": "2026-09-01T14:00:00Z",
            "charging_rate_unit": "W",
            "charging_schedule_period": [
                {"start_period": 0, "limit": "900719925474099.1", "number_phases": 4},
                {"start_period": 60, "limit": "0"}
            ],
            "min_charging_rate": "1.1"
        }
    });
    let typed: CommandResult = serde_json::from_value(result.clone()).unwrap();
    let serialized = serde_json::to_value(typed).unwrap();
    assert_eq!(
        serialized["composite_schedule_16"],
        result["composite_schedule_16"]
    );
    assert_schedule_and_exports_valid(&serialized);

    let schema = published("command-result");
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    for invalid_limit in [json!(900_719_925_474_099.1), json!("1e3")] {
        let mut invalid = serialized.clone();
        invalid["composite_schedule_16"]["charging_schedule"]["charging_schedule_period"][0]["limit"] =
            invalid_limit;
        assert!(!validator.is_valid(&invalid));
    }

    result["lifecycle"] = json!({
        "stage": "protocol_response", "accepted": false,
        "error": {"code": "protocol_rejected"}
    });
    result["composite_schedule_16"] = json!({
        "request": {"connector_id": 0, "duration": 300},
        "status": "Rejected"
    });
    let rejected: CommandResult = serde_json::from_value(result.clone()).unwrap();
    let serialized = serde_json::to_value(rejected).unwrap();
    assert_eq!(
        serialized["composite_schedule_16"],
        result["composite_schedule_16"]
    );
    assert_schedule_and_exports_valid(&serialized);

    let legacy: CommandResult = serde_json::from_value(fixtures[1].clone()).unwrap();
    assert_eq!(legacy.composite_schedule_16, None);
    let serialized = serde_json::to_value(legacy).unwrap();
    assert!(serialized.get("composite_schedule_16").is_none());
    assert_schedule_and_exports_valid(&serialized);
}

fn assert_schedule_and_exports_valid(result: &Value) {
    assert_valid("command-result", result);
    for previous in [
        include_str!("../../schemas/v1.0/command-result.schema.json"),
        include_str!("../../schemas/v1.1/command-result.schema.json"),
        include_str!("../../schemas/v1.2/command-result.schema.json"),
        include_str!("../../schemas/v1.3/command-result.schema.json"),
    ] {
        let schema: Value = serde_json::from_str(previous).unwrap();
        jsonschema::draft202012::new(&schema)
            .unwrap()
            .validate(result)
            .unwrap();
    }
    let mut batch: Value =
        serde_json::from_str(include_str!("../fixtures/export-batch-v1.json")).unwrap();
    batch["records"][0]["payload"] = json!({"kind": "command_result", "data": result});
    let typed: ExportBatch = serde_json::from_value(batch).unwrap();
    let batch = serde_json::to_value(typed).unwrap();
    assert_eq!(batch["records"][0]["payload"]["data"], *result);
    assert_valid("export-record", &batch["records"][0]);
    assert_valid("export-batch", &batch);
    for (record_schema, batch_schema) in [
        (
            include_str!("../../schemas/v1.0/export-record.schema.json"),
            include_str!("../../schemas/v1.0/export-batch.schema.json"),
        ),
        (
            include_str!("../../schemas/v1.1/export-record.schema.json"),
            include_str!("../../schemas/v1.1/export-batch.schema.json"),
        ),
        (
            include_str!("../../schemas/v1.2/export-record.schema.json"),
            include_str!("../../schemas/v1.2/export-batch.schema.json"),
        ),
        (
            include_str!("../../schemas/v1.3/export-record.schema.json"),
            include_str!("../../schemas/v1.3/export-batch.schema.json"),
        ),
        (
            include_str!("../../schemas/v1.4/export-record.schema.json"),
            include_str!("../../schemas/v1.4/export-batch.schema.json"),
        ),
    ] {
        let record_schema: Value = serde_json::from_str(record_schema).unwrap();
        let batch_schema: Value = serde_json::from_str(batch_schema).unwrap();
        jsonschema::draft202012::new(&record_schema)
            .unwrap()
            .validate(&batch["records"][0])
            .unwrap();
        jsonschema::draft202012::new(&batch_schema)
            .unwrap()
            .validate(&batch)
            .unwrap();
    }
}
