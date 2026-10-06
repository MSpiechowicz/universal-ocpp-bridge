use uob_sim::scenario::{StationResource, StationState, parse_configuration, parse_scenario};

fn scenario(action: &str, binding: &str, payload: &str) -> String {
    format!(
        "schema_version=1\nseed=113\n[[steps]]\nid='source'\nstation='alpha'\naction='{action}'\ntimeout_ms=5000\nfixture_id='wire.ocpp16.transaction-start.valid'\n{binding}\npayload={payload}\n"
    )
}

#[test]
fn live_source_binding_is_explicit_and_cannot_overwrite_authored_station_evidence() {
    let live = parse_scenario(&scenario(
        "start_transaction",
        "use_current_timestamp=true",
        "{connectorId=1,idTag='native-owner',meterStart=113}",
    ))
    .unwrap();
    assert!(live.steps[0].use_current_timestamp);
    assert!(
        live.steps[0]
            .payload
            .as_ref()
            .unwrap()
            .get("timestamp")
            .is_none()
    );

    let historical = parse_scenario(&scenario(
        "start_transaction",
        "",
        "{connectorId=1,idTag='native-owner',meterStart=113,timestamp='2026-10-05T00:00:00Z'}",
    ))
    .unwrap();
    assert!(!historical.steps[0].use_current_timestamp);
    assert_eq!(
        historical.steps[0].payload.as_ref().unwrap()["timestamp"],
        "2026-10-05T00:00:00Z"
    );

    let conflict = parse_scenario(&scenario(
        "start_transaction",
        "use_current_timestamp=true",
        "{connectorId=1,idTag='native-owner',meterStart=113,timestamp='2026-10-05T00:00:00Z'}",
    ))
    .unwrap_err();
    assert_eq!(conflict.code, "invalid_current_timestamp");

    let wrong_action = parse_scenario(&scenario(
        "authorize",
        "use_current_timestamp=true",
        "{idTag='native-owner'}",
    ))
    .unwrap_err();
    assert_eq!(wrong_action.code, "invalid_current_timestamp");
}

fn stop(binding: &str, payload: &str) -> String {
    format!(
        "schema_version=1\nseed=113\n[[steps]]\nid='stop'\nstation='alpha'\naction='stop_transaction'\ntimeout_ms=5000\nfixture_id='wire.ocpp16.transaction-stop.valid'\n{binding}\npayload={payload}\n"
    )
}

#[test]
fn active_transaction_binding_is_explicit_and_cannot_overwrite_an_authored_identifier() {
    let bound = parse_scenario(&stop(
        "use_active_transaction=true\nuse_current_timestamp=true",
        "{meterStop=114,reason='Local'}",
    ))
    .unwrap();
    assert!(bound.steps[0].use_active_transaction);
    assert!(bound.steps[0].use_current_timestamp);
    let conflict = parse_scenario(&stop(
        "use_active_transaction=true",
        "{meterStop=114,timestamp='2026-10-05T00:00:00Z',transactionId=7}",
    ))
    .unwrap_err();
    assert_eq!(conflict.code, "invalid_active_transaction");
    let wrong_action = parse_scenario(&scenario(
        "start_transaction",
        "use_active_transaction=true",
        "{connectorId=1,idTag='native-owner',meterStart=113}",
    ))
    .unwrap_err();
    assert_eq!(wrong_action.code, "invalid_active_transaction");
}

#[test]
fn only_a_single_actual_transaction_can_be_bound_by_a_stop() {
    let configuration = parse_configuration(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://127.0.0.1:9'\nocpp_version='1.6'\nconnectors=[1,2]\nrequest_timeout_ms=5000\n",
    )
    .unwrap();
    let mut state = StationState::from_definition(&configuration.stations[0]);
    assert_eq!(state.single_active_transaction(), None);
    let one = StationResource::Connector { connector_id: 1 };
    let two = StationResource::Connector { connector_id: 2 };
    state.start_transaction(one, "41").unwrap();
    assert_eq!(state.single_active_transaction(), Some("41"));
    state.start_transaction(two, "42").unwrap();
    assert_eq!(state.single_active_transaction(), None);
    state.stop_transaction(one).unwrap();
    assert_eq!(state.single_active_transaction(), Some("42"));
}
