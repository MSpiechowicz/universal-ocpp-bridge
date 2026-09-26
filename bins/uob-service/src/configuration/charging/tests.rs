use super::*;
use std::fmt::Write as _;

const BASE: &str =
    "[bridge]\nid='bridge-1'\nenvironment='demo'\n[management]\nlisten_addr='127.0.0.1:8080'\n";
const CHARGING: &str = "[charging]\nenabled=true\nlisten_addr='127.0.0.1:9000'\nstate_directory='/var/lib/uob-demo/private'\nread_grant_file='/run/uob-demo/read-grant'\n[[charging.stations]]\nid='station-a'\nprotocol='ocpp16j'\ncredential_file='/run/uob-demo/station-a'\n[[charging.stations.resources]]\nconnector_id='connector-1'\nnative_connector_id=1\n";

fn validate_document(
    document: &str,
) -> Result<Option<ValidatedChargingConfiguration>, ConfigurationLoadError> {
    let config: super::super::FileConfiguration =
        toml::from_str(document).map_err(|_| ConfigurationLoadError::InvalidCharging)?;
    config.charging.validate(
        config.bridge.environment,
        config.management.listen_addr,
        &config.bridge.id,
    )
}

#[test]
fn disabled_section_does_not_change_existing_configurations() {
    assert!(validate_document(BASE).unwrap().is_none());
    let full = super::super::validate(toml::from_str(BASE).unwrap()).unwrap();
    assert!(full.charging.is_none());
    assert!(
        validate_document(&format!("{BASE}[charging]\nenabled=false\n"))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        validate_document(&format!(
            "{BASE}[charging]\nenabled=false\nlisten_addr='127.0.0.1:9000'\n"
        ))
        .err(),
        Some(ConfigurationLoadError::InvalidCharging),
    );
}

#[test]
fn topology_provides_one_station_ref_and_native_mappings_for_both_editions() {
    let document = format!(
        "{BASE}{CHARGING}[[charging.stations]]\nid='station-b'\nprotocol='ocpp201'\ncredential_file='/run/uob-demo/station-b'\n[[charging.stations.resources]]\nevse_id='evse-a'\nnative_evse_id=1\n[[charging.stations.resources]]\nevse_id='evse-a'\nconnector_id='a-1'\nnative_evse_id=1\nnative_connector_id=1\n[[charging.stations.resources]]\nevse_id='evse-b'\nconnector_id='b-1'\nnative_evse_id=2\nnative_connector_id=1\n"
    );
    let config = validate_document(&document).unwrap().unwrap();
    assert_eq!(config.stations.len(), 2);
    assert_eq!(config.stations[0].resources.len(), 2);
    assert_eq!(config.stations[1].resources.len(), 4);
    assert_eq!(config.stations[1].resources[0].resource, None);
    assert_eq!(
        config.stations[1].resources[0].native_protocol_reference,
        None
    );
    assert_eq!(
        config.stations[1].resources[0].bridge_id.as_str(),
        "bridge-1"
    );
    assert_eq!(
        config.stations[1].resources[0].station_id.as_str(),
        "station-b"
    );
    assert_eq!(
        config.stations[1].resources[3].native_protocol_reference,
        Some(NativeProtocolReference::Ocpp201 {
            evse_id: 2,
            connector_id: Some(1)
        })
    );
}

#[test]
fn unsafe_bind_and_non_demo_listener_are_rejected() {
    for document in [
        CHARGING.replace("127.0.0.1:9000", "0.0.0.0:9000"),
        CHARGING.replace("127.0.0.1:9000", "127.0.0.1:8080"),
        CHARGING.replace("127.0.0.1:9000", "127.0.0.1:0"),
    ] {
        assert_eq!(
            validate_document(&format!("{BASE}{document}")).err(),
            Some(ConfigurationLoadError::InvalidCharging)
        );
    }
    for environment in ["production", "staging"] {
        let document = format!(
            "{}{}",
            BASE.replace(
                "environment='demo'",
                &format!("environment='{environment}'")
            ),
            CHARGING
        );
        assert_eq!(
            validate_document(&document).err(),
            Some(ConfigurationLoadError::InvalidCharging)
        );
    }
}

#[test]
fn missing_or_shared_references_cannot_enable_charging() {
    for document in [
        CHARGING.replace("read_grant_file='/run/uob-demo/read-grant'\n", ""),
        CHARGING.replace("state_directory='/var/lib/uob-demo/private'\n", ""),
        CHARGING.replace("credential_file='/run/uob-demo/station-a'\n", ""),
        CHARGING.replace("/run/uob-demo/station-a", "/run/uob-demo/read-grant"),
        CHARGING.replace("/run/uob-demo/station-a", "relative/secret"),
        CHARGING.replace("/run/uob-demo/station-a", "/run/uob-demo/../secret"),
        CHARGING.replace(
            "/run/uob-demo/station-a",
            "/var/lib/uob-demo/private/secret",
        ),
        CHARGING.replace("/var/lib/uob-demo/private", "relative/state"),
        CHARGING.replace(
            "[[charging.stations.resources]]\nconnector_id='connector-1'\nnative_connector_id=1\n",
            "",
        ),
    ] {
        let document = format!("{BASE}{document}");
        assert!(
            validate_document(&document).is_err(),
            "must reject {document}"
        );
    }
}

#[test]
fn duplicate_station_native_or_canonical_address_is_rejected() {
    let second =
        "[[charging.stations.resources]]\nconnector_id='connector-2'\nnative_connector_id=1\n";
    let duplicate_native = format!("{BASE}{CHARGING}{second}");
    assert_eq!(
        validate_document(&duplicate_native).err(),
        Some(ConfigurationLoadError::InvalidCharging)
    );
    let duplicate_canonical = format!(
        "{BASE}{CHARGING}{}",
        second
            .replace("connector-2", "connector-1")
            .replace("native_connector_id=1", "native_connector_id=2")
    );
    assert_eq!(
        validate_document(&duplicate_canonical).err(),
        Some(ConfigurationLoadError::InvalidCharging)
    );
    let duplicate_station = format!(
        "{BASE}{CHARGING}[[charging.stations]]\nid='station-a'\nprotocol='ocpp201'\ncredential_file='/run/uob-demo/station-b'\n[[charging.stations.resources]]\nevse_id='evse-2'\nnative_evse_id=1\n"
    );
    assert_eq!(
        validate_document(&duplicate_station).err(),
        Some(ConfigurationLoadError::InvalidCharging)
    );
    let duplicate_evse_connector = format!("{BASE}{}[[charging.stations.resources]]\nevse_id='evse-b'\nconnector_id='connector-b'\nnative_evse_id=1\nnative_connector_id=1\n", CHARGING.replace("protocol='ocpp16j'", "protocol='ocpp201'").replace("connector_id='connector-1'\nnative_connector_id=1", "evse_id='evse-a'\nconnector_id='connector-a'\nnative_evse_id=1\nnative_connector_id=1"));
    assert_eq!(
        validate_document(&duplicate_evse_connector).err(),
        Some(ConfigurationLoadError::InvalidCharging)
    );
}

#[test]
fn edition_specific_addresses_and_extraneous_fields_are_rejected() {
    for document in [
        CHARGING.replace("native_connector_id=1", "native_connector_id=0"),
        CHARGING.replace(
            "native_connector_id=1",
            "native_connector_id=1\nnative_evse_id=1",
        ),
        CHARGING.replace("protocol='ocpp16j'", "protocol='ocpp201'"),
        CHARGING
            .replace("protocol='ocpp16j'", "protocol='ocpp201'")
            .replace(
                "connector_id='connector-1'\nnative_connector_id=1",
                "evse_id='evse-a'\nnative_evse_id=0",
            ),
        CHARGING
            .replace("protocol='ocpp16j'", "protocol='ocpp201'")
            .replace(
                "connector_id='connector-1'\nnative_connector_id=1",
                "evse_id='evse-a'\nnative_evse_id=1\nnative_connector_id=1",
            ),
    ] {
        assert_eq!(
            validate_document(&format!("{BASE}{document}")).err(),
            Some(ConfigurationLoadError::InvalidCharging)
        );
    }
    let unknown = format!(
        "{BASE}{}",
        CHARGING.replace(
            "native_connector_id=1",
            "native_connector_id=1\ncredential='embedded-value'"
        )
    );
    assert!(toml::from_str::<super::super::FileConfiguration>(&unknown).is_err());
}

#[test]
fn station_limit_and_credential_uniqueness_are_enforced() {
    let repeated_credential = format!(
        "{BASE}{CHARGING}[[charging.stations]]\nid='station-b'\nprotocol='ocpp201'\ncredential_file='/run/uob-demo/station-a'\n[[charging.stations.resources]]\nevse_id='evse-b'\nnative_evse_id=1\n"
    );
    assert_eq!(
        validate_document(&repeated_credential).err(),
        Some(ConfigurationLoadError::InvalidCharging)
    );

    let mut many = format!("{BASE}{CHARGING}");
    for index in 1..=DEFAULT_MAX_CONNECTED_STATIONS {
        write!(
            many,
            "[[charging.stations]]\nid='station-{index}'\nprotocol='ocpp16j'\ncredential_file='/run/uob-demo/station-{index}'\n[[charging.stations.resources]]\nconnector_id='connector-{index}'\nnative_connector_id=1\n"
        )
        .unwrap();
    }
    assert_eq!(
        validate_document(&many).err(),
        Some(ConfigurationLoadError::InvalidCharging)
    );
}

#[test]
fn opt_in_control_grants_and_local_token_are_distinct_private_references() {
    let enabled = CHARGING
        .replace("read_grant_file='/run/uob-demo/read-grant'",
            "read_grant_file='/run/uob-demo/read-grant'\ncontrol_grant_file='/run/uob-demo/control'\nprivileged_grant_file='/run/uob-demo/privileged'")
        .replace("credential_file='/run/uob-demo/station-a'",
            "credential_file='/run/uob-demo/station-a'\nstart_token_file='/run/uob-demo/start-token'\nallow_stop=true\nallow_charging_limit=true\nchange_availability=true");
    let configured = validate_document(&format!("{BASE}{enabled}"))
        .unwrap()
        .unwrap();
    assert!(configured.control_grant_file.is_some());
    assert!(configured.privileged_grant_file.is_some());
    assert!(configured.stations[0].start_token_file.is_some());
    assert!(configured.stations[0].allow_charging_limit);
    for invalid in [
        enabled.replace("/run/uob-demo/privileged", "/run/uob-demo/control"),
        enabled.replace("/run/uob-demo/start-token", "/run/uob-demo/read-grant"),
        enabled.replace(
            "/run/uob-demo/start-token",
            "/var/lib/uob-demo/private/start-token",
        ),
        enabled.replace("control_grant_file='/run/uob-demo/control'\n", ""),
        enabled.replace("privileged_grant_file='/run/uob-demo/privileged'\n", ""),
    ] {
        assert_eq!(
            validate_document(&format!("{BASE}{invalid}")).err(),
            Some(ConfigurationLoadError::InvalidCharging),
        );
    }
    assert!(validate_document(&format!("{BASE}{CHARGING}")).is_ok());
}

#[test]
fn evse_identity_cannot_be_reassigned_to_a_second_native_evse() {
    let first = CHARGING
        .replace("protocol='ocpp16j'", "protocol='ocpp201'")
        .replace(
            "connector_id='connector-1'\nnative_connector_id=1",
            "evse_id='evse-1'\nconnector_id='connector-1'\nnative_evse_id=1\nnative_connector_id=1",
        );
    let reassigned = format!(
        "{BASE}{first}[[charging.stations.resources]]\nevse_id='evse-1'\nconnector_id='connector-2'\nnative_evse_id=2\nnative_connector_id=1\n"
    );
    assert_eq!(
        validate_document(&reassigned).err(),
        Some(ConfigurationLoadError::InvalidCharging)
    );
}
