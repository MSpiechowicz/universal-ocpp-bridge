use super::*;

#[test]
fn native201_results_and_embedded_exports_keep_exact_quantities_and_distinct_statuses() {
    let fixtures: Value =
        serde_json::from_str(include_str!("../fixtures/command-results-v1.json")).unwrap();
    let mut result = fixtures[1].clone();
    result["schema_version"]["revision"] = json!(7);
    result["charging_profile_201"] = json!({
        "action":"SetChargingProfile", "status":"Accepted", "request": {
            "evse_id":1, "charging_profile": {"id":i32::MIN, "stack_level":2,
                "charging_profile_purpose":"TxDefaultProfile", "charging_profile_kind":"Absolute",
                "valid_from":"2026-09-01T00:00:00Z", "valid_to":"2026-10-01T00:00:00Z",
                "charging_schedule":[{"id":i32::MAX, "start_schedule":"2026-09-01T00:00:00Z",
                    "duration":0, "charging_rate_unit":"W", "min_charging_rate":"0",
                    "charging_schedule_period":[{"start_period":0,"limit":"900719925474099.1"},
                        {"start_period":60,"limit":"0","number_phases":1,"phase_to_use":3}]}]
            }
        }
    });
    let typed: CommandResult = serde_json::from_value(result.clone()).unwrap();
    assert!(typed.charging_profile_201.as_ref().unwrap().accepted());
    assert_eq!(
        serde_json::to_value(typed).unwrap()["charging_profile_201"],
        result["charging_profile_201"]
    );
    assert_valid("command-result", &result);
    let mut batch: Value =
        serde_json::from_str(include_str!("../fixtures/export-batch-v1.json")).unwrap();
    batch["records"][0]["payload"] = json!({"kind":"command_result", "data":result});
    assert_valid("export-record", &batch["records"][0]);
    assert_valid("export-batch", &batch);
    for status in ["Accepted", "Unknown"] {
        let mut clear = fixtures[1].clone();
        clear["schema_version"]["revision"] = json!(7);
        clear["charging_profile_201"] = json!({"action":"ClearChargingProfile","status":status,
            "request":{"charging_profile_criteria":{"charging_profile_purpose":"TxProfile"}}});
        let typed: CommandResult = serde_json::from_value(clear.clone()).unwrap();
        assert_eq!(
            typed.charging_profile_201.as_ref().unwrap().accepted(),
            status == "Accepted"
        );
        assert_valid("command-result", &clear);
    }
    let mut invalid = result.clone();
    invalid["charging_profile_201"]["status"] = json!("NotSupported");
    assert!(
        !jsonschema::draft202012::new(&published("command-result"))
            .unwrap()
            .is_valid(&invalid)
    );
    invalid = result;
    invalid["charging_profile_201"]["request"]["charging_profile"]["charging_schedule"][0]["charging_schedule_period"]
        [0]["limit"] = json!(8.1);
    assert!(
        !jsonschema::draft202012::new(&published("command-result"))
            .unwrap()
            .is_valid(&invalid)
    );
}

#[test]
fn native201_evidence_is_optional_for_all_released_readers() {
    for (name, previous) in [
        (
            "command-result",
            include_str!("../../schemas/v1.6/command-result.schema.json"),
        ),
        (
            "export-record",
            include_str!("../../schemas/v1.7/export-record.schema.json"),
        ),
        (
            "export-batch",
            include_str!("../../schemas/v1.7/export-batch.schema.json"),
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
    }
    let fixtures: Value =
        serde_json::from_str(include_str!("../fixtures/command-results-v1.json")).unwrap();
    for historical in fixtures.as_array().unwrap() {
        let typed: CommandResult = serde_json::from_value(historical.clone()).unwrap();
        assert_eq!(typed.charging_profile_201, None);
        assert!(
            serde_json::to_value(typed)
                .unwrap()
                .get("charging_profile_201")
                .is_none()
        );
        assert_valid("command-result", historical);
    }
}
