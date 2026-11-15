//! `extended_trigger_message`: OCPP 1.6 only, off by default, needs the privileged grant.
use super::*;

const BASE: &str =
    "[bridge]\nid='bridge-1'\nenvironment='demo'\n[management]\nlisten_addr='127.0.0.1:8080'\n";
const GRANTS: &str = "read_grant_file='/run/uob-demo/read-grant'\ncontrol_grant_file='/run/uob-demo/control'\nprivileged_grant_file='/run/uob-demo/privileged'\n";

fn station(protocol: &str, options: &str, grants: &str) -> String {
    let resource = if protocol == "ocpp201" {
        "evse_id='evse-1'\nconnector_id='connector-1'\nnative_evse_id=1\nnative_connector_id=1\n"
    } else {
        "connector_id='connector-1'\nnative_connector_id=1\n"
    };
    format!(
        "{BASE}[charging]\nenabled=true\nlisten_addr='127.0.0.1:9000'\n\
         state_directory='/var/lib/uob-demo/private'\n{grants}\
         [[charging.stations]]\nid='station-a'\nprotocol='{protocol}'\n\
         credential_file='/run/uob-demo/station-a'\n{options}\
         [[charging.stations.resources]]\n{resource}"
    )
}

fn validate(
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
fn the_option_is_off_by_default_and_parses_as_one_boolean_key() {
    let default = validate(&station(
        "ocpp16j",
        "",
        "read_grant_file='/run/uob-demo/read-grant'\n",
    ))
    .unwrap()
    .unwrap();
    let control = default.stations[0].control;
    assert!(!control.extended_trigger_message.enabled());
    assert!(!control.trigger_enabled());

    let enabled = validate(&station(
        "ocpp16j",
        "extended_trigger_message=true\n",
        GRANTS,
    ))
    .unwrap()
    .unwrap();
    let control = enabled.stations[0].control;
    assert!(control.extended_trigger_message.enabled());
    // Either trigger action opens the evidence window; the plain one stays independent.
    assert!(control.trigger_enabled());
    assert!(!control.trigger_message.enabled());

    let explicit = validate(&station(
        "ocpp16j",
        "extended_trigger_message=false\n",
        GRANTS,
    ))
    .unwrap()
    .unwrap();
    assert!(
        !explicit.stations[0]
            .control
            .extended_trigger_message
            .enabled()
    );
    for invalid in [
        "extended_trigger_message='yes'\n",
        "extended_trigger_message=1\n",
    ] {
        assert_eq!(
            validate(&station("ocpp16j", invalid, GRANTS)).err(),
            Some(ConfigurationLoadError::InvalidCharging),
            "{invalid}"
        );
    }
}

#[test]
fn it_needs_the_privileged_grant_like_every_other_remote_action() {
    for grants in [
        "read_grant_file='/run/uob-demo/read-grant'\n",
        "read_grant_file='/run/uob-demo/read-grant'\ncontrol_grant_file='/run/uob-demo/control'\n",
    ] {
        assert_eq!(
            validate(&station(
                "ocpp16j",
                "extended_trigger_message=true\n",
                grants
            ))
            .err(),
            Some(ConfigurationLoadError::InvalidCharging),
        );
    }
    let plain = station("ocpp16j", "trigger_message=true\n", GRANTS);
    assert!(validate(&plain).unwrap().is_some());
}

#[test]
fn an_ocpp201_station_cannot_enable_it() {
    let document = station("ocpp201", "extended_trigger_message=true\n", GRANTS);
    assert_eq!(
        validate(&document).err(),
        Some(ConfigurationLoadError::InvalidCharging)
    );
    // The same station without the option stays valid, and the plain action is unaffected.
    let plain = station("ocpp201", "trigger_message=true\n", GRANTS);
    assert!(validate(&plain).unwrap().is_some());
}
