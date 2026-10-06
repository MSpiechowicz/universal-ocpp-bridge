use uob_sim::scenario::{parse_configuration, parse_scenario};

#[test]
fn examples_parse_and_bind_native_reservation_state_to_the_station() {
    let configuration =
        parse_configuration(include_str!("../examples/reservations-2.0.1-config.toml")).unwrap();
    let station = &configuration.stations[0];
    let options = station.reservation201.as_ref().unwrap();
    assert!(options.enabled && options.non_evse_specific);
    assert_eq!(options.connector_types.len(), 2);
    let client = station.client_config();
    assert_eq!(client.reservation201.as_ref().unwrap().0, "alpha");
    assert!(client.reservation16.is_none());
    assert_eq!(client.evse_connectors, vec![(1, 1), (2, 1)]);
    let scenario = parse_scenario(include_str!("../examples/reservations-2.0.1.toml")).unwrap();
    assert_eq!(scenario.seed, 114);
    assert!(
        scenario
            .steps
            .iter()
            .any(|step| step.action.name() == "await_reservation")
    );
}

#[test]
fn native_reservation_state_must_match_the_station_edition() {
    for (edition, topology, table) in [
        ("1.6", "connectors=[1]", "reservation201"),
        (
            "2.0.1",
            "[[stations.evses]]\nid=1\nconnectors=[1]",
            "reservation16",
        ),
    ] {
        let source = format!(
            "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://127.0.0.1:9'\nocpp_version='{edition}'\n{topology}\n[stations.{table}]\nprivate_state_file='/tmp/unused'\nenabled=true\n"
        );
        assert_eq!(
            parse_configuration(&source).unwrap_err().code,
            "invalid_reservation_edition"
        );
    }
    let unknown = "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://127.0.0.1:9'\nocpp_version='2.0.1'\n[stations.reservation201]\nprivate_state_file='/tmp/unused'\nreserve_connector_zero_supported=true\n";
    assert_eq!(
        parse_configuration(unknown).unwrap_err().code,
        "invalid_configuration_toml"
    );
}
