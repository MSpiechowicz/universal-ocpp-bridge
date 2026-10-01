use uob_contracts::CommandResult;

#[test]
fn old_command_results_read_without_schedule_evidence() {
    let results: Vec<CommandResult> =
        serde_json::from_slice(include_bytes!("fixtures/command-results-v1.json")).unwrap();
    for result in results {
        assert!(result.composite_schedule_16.is_none());
        assert!(
            serde_json::to_value(result)
                .unwrap()
                .get("composite_schedule_16")
                .is_none()
        );
    }
}
