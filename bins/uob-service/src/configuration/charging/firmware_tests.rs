use super::super::{ConfigurationLoadError, ValidatedChargingConfiguration};
use std::time::Duration;

const BASE: &str =
    "[bridge]\nid='bridge-1'\nenvironment='demo'\n[management]\nlisten_addr='127.0.0.1:8080'\n";
const CHARGING: &str = "[charging]\nenabled=true\nlisten_addr='127.0.0.1:9000'\nstate_directory='/var/lib/uob-demo/private'\nread_grant_file='/run/uob-demo/read-grant'\ncontrol_grant_file='/run/uob-demo/control-grant'\nprivileged_grant_file='/run/uob-demo/privileged-grant'\n";
const SECTION: &str = "[charging.firmware]\nlisten_addr='127.0.0.1:9100'\nspool_directory='/var/lib/uob-demo/spool'\ncatalog_file='/run/uob-demo/firmware.json'\n";

fn station(protocol: &str, options: &str) -> String {
    format!(
        "[[charging.stations]]\nid='station-a'\nprotocol='{protocol}'\ncredential_file='/run/uob-demo/station-a'\n{options}\n[[charging.stations.resources]]\nconnector_id='connector-1'\nnative_connector_id=1\n"
    )
}

fn station_201(protocol: &str, options: &str) -> String {
    let station = station(protocol, options);
    if protocol == "ocpp201" {
        station.replace(
            "connector_id='connector-1'\nnative_connector_id=1",
            "evse_id='evse-1'\nnative_evse_id=1",
        )
    } else {
        station
    }
}

fn validate(
    station: &str,
    section: &str,
) -> Result<Option<ValidatedChargingConfiguration>, ConfigurationLoadError> {
    let document = format!("{BASE}{CHARGING}{section}{station}");
    let config: super::super::super::FileConfiguration =
        toml::from_str(&document).map_err(|_| ConfigurationLoadError::InvalidCharging)?;
    config.charging.validate(
        config.bridge.environment,
        config.management.listen_addr,
        &config.bridge.id,
    )
}

#[test]
fn exactly_one_native_family_with_a_bounded_job_deadline_and_a_used_artifact_service() {
    let legacy = validate(
        &station(
            "ocpp16j",
            "update_firmware=true\nfirmware_job_timeout_seconds=3600",
        ),
        SECTION,
    )
    .unwrap()
    .unwrap();
    let firmware = legacy.stations[0].firmware.unwrap();
    assert!(!firmware.signed);
    assert_eq!(firmware.job_timeout, Duration::from_secs(3600));
    let artifacts = legacy.firmware.unwrap();
    assert_eq!(artifacts.public_base, "http://127.0.0.1:9100");
    assert!(artifacts.manufacturer_root_file.is_none());
    let signed = validate(
        &station(
            "ocpp16j",
            "signed_update_firmware=true\nfirmware_job_timeout_seconds=60",
        ),
        SECTION,
    )
    .unwrap()
    .unwrap();
    assert!(signed.stations[0].firmware.unwrap().signed);
}

#[test]
fn ocpp201_update_is_secure_unless_explicitly_non_secure() {
    let secure = validate(
        &station_201(
            "ocpp201",
            "update_firmware=true\nfirmware_job_timeout_seconds=600",
        ),
        SECTION,
    )
    .unwrap()
    .unwrap();
    let firmware = secure.stations[0].firmware.unwrap();
    assert!(firmware.signed);
    assert_eq!(firmware.job_timeout, Duration::from_secs(600));
    let non_secure = validate(
        &station_201(
            "ocpp201",
            "update_firmware=true\nnon_secure_firmware=true\nfirmware_job_timeout_seconds=600",
        ),
        SECTION,
    )
    .unwrap()
    .unwrap();
    assert!(!non_secure.stations[0].firmware.unwrap().signed);
}

#[test]
fn ambiguous_unbounded_or_unused_firmware_options_are_rejected() {
    let invalid = ConfigurationLoadError::InvalidCharging;
    for (options, section) in [
        // Both families on one station (L01.FR.20).
        (
            "update_firmware=true\nsigned_update_firmware=true\nfirmware_job_timeout_seconds=600",
            SECTION,
        ),
        ("update_firmware=true", SECTION),
        (
            "update_firmware=true\nfirmware_job_timeout_seconds=59",
            SECTION,
        ),
        (
            "update_firmware=true\nfirmware_job_timeout_seconds=604801",
            SECTION,
        ),
        ("firmware_job_timeout_seconds=600", ""),
        ("update_firmware=true\nfirmware_job_timeout_seconds=600", ""),
        ("", SECTION),
    ] {
        assert_eq!(
            validate(&station("ocpp16j", options), section).err(),
            Some(invalid),
            "{options} / {section}"
        );
    }
    for (protocol, options) in [
        // The Security Whitepaper message does not exist in OCPP 2.0.1.
        (
            "ocpp201",
            "signed_update_firmware=true\nfirmware_job_timeout_seconds=600",
        ),
        // L02 selection is OCPP 2.0.1 only and requires the action itself.
        (
            "ocpp16j",
            "update_firmware=true\nnon_secure_firmware=true\nfirmware_job_timeout_seconds=600",
        ),
        ("ocpp201", "non_secure_firmware=true"),
        ("ocpp201", "update_firmware=true"),
    ] {
        assert_eq!(
            validate(&station_201(protocol, options), SECTION).err(),
            Some(invalid),
            "{protocol} {options}"
        );
    }
    for section in [
        SECTION.replace("127.0.0.1:9100", "0.0.0.0:9100"),
        SECTION.replace("127.0.0.1:9100", "127.0.0.1:9000"),
        SECTION.replace("127.0.0.1:9100", "127.0.0.1:8080"),
        SECTION.replace("/var/lib/uob-demo/spool", "/var/lib/uob-demo/private/spool"),
        SECTION.replace("/run/uob-demo/firmware.json", "relative.json"),
        format!("{SECTION}unknown=true\n"),
    ] {
        assert_eq!(
            validate(
                &station(
                    "ocpp16j",
                    "update_firmware=true\nfirmware_job_timeout_seconds=600"
                ),
                &section
            )
            .err(),
            Some(invalid),
            "{section}"
        );
    }
}

#[test]
fn firmware_requires_the_privileged_grant() {
    let document = format!(
        "{BASE}{}{SECTION}{}",
        CHARGING.replace(
            "privileged_grant_file='/run/uob-demo/privileged-grant'\n",
            ""
        ),
        station(
            "ocpp16j",
            "update_firmware=true\nfirmware_job_timeout_seconds=600"
        )
    );
    let config: super::super::super::FileConfiguration = toml::from_str(&document).unwrap();
    assert_eq!(
        config
            .charging
            .validate(
                config.bridge.environment,
                config.management.listen_addr,
                &config.bridge.id
            )
            .err(),
        Some(ConfigurationLoadError::InvalidCharging)
    );
}
