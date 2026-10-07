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

const UPLOAD_SECTION: &str = "[charging.firmware]\nlisten_addr='127.0.0.1:9100'\nspool_directory='/var/lib/uob-demo/spool'\n";

#[test]
fn log_uploads_use_the_artifact_service_without_a_firmware_catalog() {
    let both = validate(
        &station(
            "ocpp16j",
            "get_diagnostics=true\nget_log=true\ndiagnostics_job_timeout_seconds=600",
        ),
        UPLOAD_SECTION,
    )
    .unwrap()
    .unwrap();
    let diagnostics = both.stations[0].diagnostics.unwrap();
    assert!(diagnostics.diagnostics && diagnostics.log);
    assert_eq!(diagnostics.job_timeout, Duration::from_secs(600));
    assert_eq!(diagnostics.maximum_upload_bytes, 8 * 1024 * 1024);
    let artifacts = both.firmware.unwrap();
    assert!(artifacts.catalog_file.is_none());
    assert_eq!(artifacts.upload_stations, 1);
    let log_only = validate(
        &station(
            "ocpp16j",
            "get_log=true\ndiagnostics_job_timeout_seconds=60\ndiagnostics_upload_max_bytes=1",
        ),
        UPLOAD_SECTION,
    )
    .unwrap()
    .unwrap();
    let diagnostics = log_only.stations[0].diagnostics.unwrap();
    assert!(!diagnostics.diagnostics && diagnostics.log);
    assert_eq!(diagnostics.maximum_upload_bytes, 1);
    // A station may combine firmware and log uploads on the same service.
    assert!(
        validate(
            &station(
                "ocpp16j",
                "update_firmware=true\nfirmware_job_timeout_seconds=600\nget_diagnostics=true\ndiagnostics_job_timeout_seconds=600",
            ),
            SECTION,
        )
        .is_ok()
    );
}

#[test]
fn log_upload_options_are_bounded_edition_correct_and_never_unused() {
    let fail = Err(ConfigurationLoadError::InvalidCharging);
    for (protocol, options, section) in [
        // No deadline, or one outside the bounds.
        ("ocpp16j", "get_diagnostics=true", UPLOAD_SECTION),
        (
            "ocpp16j",
            "get_diagnostics=true\ndiagnostics_job_timeout_seconds=59",
            UPLOAD_SECTION,
        ),
        (
            "ocpp16j",
            "get_diagnostics=true\ndiagnostics_job_timeout_seconds=86401",
            UPLOAD_SECTION,
        ),
        (
            "ocpp16j",
            "get_log=true\ndiagnostics_job_timeout_seconds=600\ndiagnostics_upload_max_bytes=0",
            UPLOAD_SECTION,
        ),
        (
            "ocpp16j",
            "get_log=true\ndiagnostics_job_timeout_seconds=600\ndiagnostics_upload_max_bytes=33554433",
            UPLOAD_SECTION,
        ),
        // Options without an enabled family.
        (
            "ocpp16j",
            "diagnostics_job_timeout_seconds=600",
            UPLOAD_SECTION,
        ),
        ("ocpp16j", "diagnostics_upload_max_bytes=10", UPLOAD_SECTION),
        // `GetDiagnostics` is an OCPP 1.6 message; OCPP 2.0.1 retrieves logs with `GetLog`.
        (
            "ocpp201",
            "get_diagnostics=true\ndiagnostics_job_timeout_seconds=600",
            UPLOAD_SECTION,
        ),
        ("ocpp201", "get_log=true", UPLOAD_SECTION),
        (
            "ocpp201",
            "get_log=true\ndiagnostics_job_timeout_seconds=600\ndiagnostics_upload_max_bytes=0",
            UPLOAD_SECTION,
        ),
        (
            "ocpp201",
            "diagnostics_job_timeout_seconds=600",
            UPLOAD_SECTION,
        ),
        // Uploads need the artifact service.
        (
            "ocpp16j",
            "get_diagnostics=true\ndiagnostics_job_timeout_seconds=600",
            "",
        ),
        // A catalog belongs only to firmware, and firmware needs one.
        (
            "ocpp16j",
            "get_diagnostics=true\ndiagnostics_job_timeout_seconds=600",
            SECTION,
        ),
        (
            "ocpp16j",
            "update_firmware=true\nfirmware_job_timeout_seconds=600",
            UPLOAD_SECTION,
        ),
    ] {
        assert_eq!(
            validate(&station_201(protocol, options), section).map(|_| ()),
            fail,
            "{protocol} {options}"
        );
    }
}

#[test]
fn ocpp201_log_stations_use_the_artifact_service_without_a_firmware_catalog() {
    let log = validate(
        &station_201(
            "ocpp201",
            "get_log=true\ndiagnostics_job_timeout_seconds=600\ndiagnostics_upload_max_bytes=4096",
        ),
        UPLOAD_SECTION,
    )
    .unwrap()
    .unwrap();
    let diagnostics = log.stations[0].diagnostics.unwrap();
    assert!(!diagnostics.diagnostics && diagnostics.log);
    assert_eq!(diagnostics.job_timeout, Duration::from_secs(600));
    assert_eq!(diagnostics.maximum_upload_bytes, 4096);
    assert!(log.stations[0].firmware.is_none());
    let artifacts = log.firmware.unwrap();
    assert!(artifacts.catalog_file.is_none());
    assert_eq!(artifacts.upload_stations, 1);
    // Without a log or firmware station the artifact service is not allowed, and a log
    // station cannot start without it.
    assert!(validate(&station_201("ocpp201", ""), UPLOAD_SECTION).is_err());
    assert!(
        validate(
            &station_201(
                "ocpp201",
                "get_log=true\ndiagnostics_job_timeout_seconds=600"
            ),
            ""
        )
        .is_err()
    );
    // A secure firmware station and a log station share one service and one catalog.
    assert!(
        validate(
            &station_201(
                "ocpp201",
                "update_firmware=true\nfirmware_job_timeout_seconds=600\nget_log=true\ndiagnostics_job_timeout_seconds=600",
            ),
            SECTION,
        )
        .is_ok()
    );
}
