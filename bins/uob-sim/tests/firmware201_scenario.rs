//! 2.0.1 firmware station configuration and scenario actions are validated before any socket.
use uob_sim::firmware201::{CancelPolicy201, FirmwareMode201};
use uob_sim::scenario::{ActionKind, parse_configuration, parse_scenario};

const STATION: &str =
    "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://127.0.0.1:9/ocpp/alpha'\n";

fn configuration(edition: &str, firmware: &str) -> String {
    format!(
        "{STATION}ocpp_version='{edition}'\n[stations.firmware201]\nprivate_state_file='/srv/firmware.json'\n{firmware}\n"
    )
}

#[test]
fn examples_parse_and_bind_the_native_firmware_model_to_its_station() {
    let secure =
        parse_configuration(include_str!("../examples/firmware-2.0.1-config.toml")).unwrap();
    let options = secure.stations[0].firmware201.as_ref().unwrap();
    assert_eq!(
        (options.mode, options.cancel_policy),
        (FirmwareMode201::Secure, CancelPolicy201::Cancel)
    );
    let client = secure.stations[0].client_config();
    assert_eq!(client.firmware201.as_ref().unwrap().0, "alpha");
    assert!(client.reconnect && client.firmware16.is_none());
    let plain = parse_configuration(include_str!(
        "../examples/non-secure-firmware-2.0.1-config.toml"
    ))
    .unwrap();
    assert_eq!(
        plain.stations[0].firmware201.as_ref().unwrap().mode,
        FirmwareMode201::NonSecure
    );
    let scenario = parse_scenario(include_str!("../examples/firmware-2.0.1.toml")).unwrap();
    assert!(
        scenario
            .steps
            .iter()
            .any(|step| matches!(step.action, ActionKind::AwaitFirmware))
    );
}

#[test]
fn invalid_firmware_station_configuration_fails_setup() {
    for (edition, firmware) in [
        ("1.6", "mode='non_secure'"),
        ("2.0.1", "mode='secure'"),
        (
            "2.0.1",
            "mode='non_secure'\nmanufacturer_root_file='/srv/root.pem'",
        ),
        ("2.0.1", "mode='non_secure'\nmaximum_bytes=0"),
        ("2.0.1", "mode='non_secure'\ndownload_failures=17"),
        ("2.0.1", "mode='non_secure'\ncancel_policy='ignore'"),
        ("2.0.1", "mode='non_secure'\nreject_signed=true"),
        ("2.0.1", "mode='legacy'"),
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
    assert!(parse_configuration(&configuration("2.0.1", "mode='non_secure'")).is_ok());
}

#[tokio::test]
async fn a_rebooting_firmware_station_must_reconnect() {
    let without = parse_configuration(&configuration("2.0.1", "mode='non_secure'")).unwrap();
    let Err(error) =
        uob_sim::SimulatorProtocolClient::connect(without.stations[0].client_config()).await
    else {
        panic!("a rebooting firmware station without reconnect must not start");
    };
    assert!(error.to_string().contains("requires reconnect"), "{error}");
}

#[test]
fn firmware_actions_share_safe_expectations_across_editions() {
    let source = "schema_version=1\nseed=123\n[[steps]]\nid='firmware'\nstation='alpha'\naction='await_firmware'\ntimeout_ms=5000\nexpect_response={requestId=7,lastStatus='Installed'}\n";
    let scenario = parse_scenario(source).unwrap();
    assert_eq!(scenario.steps[0].action.event(), "firmware_observed");
}
