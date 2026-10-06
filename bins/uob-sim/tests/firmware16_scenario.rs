//! Firmware station configuration and scenario actions are validated before any socket opens.
use uob_sim::firmware16::{CancelPolicy, FirmwareMode};
use uob_sim::scenario::{ActionKind, parse_configuration, parse_scenario};

const STATION: &str =
    "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://127.0.0.1:9/ocpp/alpha'\n";

fn configuration(edition: &str, firmware: &str) -> String {
    format!(
        "{STATION}ocpp_version='{edition}'\n[stations.firmware16]\nprivate_state_file='/srv/firmware.json'\n{firmware}\n"
    )
}

fn step(action: &str, extra: &str) -> String {
    format!(
        "schema_version=1\nseed=122\n[[steps]]\nid='firmware'\nstation='alpha'\naction='{action}'\ntimeout_ms=5000\n{extra}\n"
    )
}

#[test]
fn examples_parse_and_bind_the_native_firmware_model_to_its_station() {
    let legacy = parse_configuration(include_str!("../examples/firmware-1.6-config.toml")).unwrap();
    let options = legacy.stations[0].firmware16.as_ref().unwrap();
    assert_eq!(options.mode, FirmwareMode::Legacy);
    let client = legacy.stations[0].client_config();
    assert_eq!(client.firmware16.as_ref().unwrap().0, "alpha");
    let signed =
        parse_configuration(include_str!("../examples/signed-firmware-1.6-config.toml")).unwrap();
    let options = signed.stations[0].firmware16.as_ref().unwrap();
    assert_eq!(
        (options.mode, options.cancel_policy),
        (FirmwareMode::Signed, CancelPolicy::Cancel)
    );
    for source in [
        include_str!("../examples/firmware-1.6.toml"),
        include_str!("../examples/signed-firmware-1.6.toml"),
    ] {
        let scenario = parse_scenario(source).unwrap();
        assert!(
            scenario
                .steps
                .iter()
                .any(|step| matches!(step.action, ActionKind::AwaitFirmware))
        );
    }
}

#[test]
fn invalid_firmware_station_configuration_fails_setup() {
    for (edition, firmware) in [
        ("2.0.1", "mode='legacy'"),
        ("1.6", "mode='signed'"),
        (
            "1.6",
            "mode='legacy'\nmanufacturer_root_file='/srv/root.pem'",
        ),
        ("1.6", "mode='legacy'\nreject_signed=true"),
        ("1.6", "mode='legacy'\nmaximum_bytes=0"),
        ("1.6", "mode='legacy'\ncancel_policy='ignore'"),
        ("1.6", "mode='legacy'\nunknown=true"),
        ("1.6", "mode='unsigned'"),
    ] {
        let failure = parse_configuration(&configuration(edition, firmware)).unwrap_err();
        assert!(
            matches!(
                failure.code,
                "invalid_firmware_configuration" | "invalid_configuration_toml"
            ),
            "{edition} {firmware}: {}",
            failure.code
        );
    }
    assert!(parse_configuration(&configuration("1.6", "mode='legacy'")).is_ok());
}

#[test]
fn firmware_actions_accept_only_safe_expectations() {
    let awaited = parse_scenario(&step(
        "await_firmware",
        "expect_response={lastStatus='Installed',reboots=1}",
    ))
    .unwrap();
    assert_eq!(awaited.steps[0].action.name(), "await_firmware");
    assert_eq!(awaited.steps[0].action.event(), "firmware_observed");
    assert!(parse_scenario(&step("assert_firmware", "expect_response={active=false}")).is_ok());
    let with_payload =
        parse_scenario(&step("assert_firmware", "payload={location='x'}")).unwrap_err();
    assert_eq!(with_payload.code, "invalid_action_payload");
    let with_event = parse_scenario(&step(
        "await_firmware",
        "expect_event='reservation_observed'",
    ))
    .unwrap_err();
    assert_eq!(with_event.code, "unsupported_expected_event");
}
