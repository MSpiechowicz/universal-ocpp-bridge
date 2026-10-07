//! Diagnostics station configuration and scenario actions are validated before any socket opens.
use uob_sim::diagnostics16::{LogCancelPolicy, LogFailureStatus};
use uob_sim::scenario::{ActionKind, parse_configuration, parse_scenario};

const STATION: &str =
    "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://127.0.0.1:9/ocpp/alpha'\n";

fn configuration(edition: &str, diagnostics: &str) -> String {
    format!(
        "{STATION}ocpp_version='{edition}'\n[stations.diagnostics16]\nprivate_state_file='/srv/diagnostics.json'\n{diagnostics}\n"
    )
}

fn step(action: &str, extra: &str) -> String {
    format!(
        "schema_version=1\nseed=124\n[[steps]]\nid='diagnostics'\nstation='alpha'\naction='{action}'\ntimeout_ms=5000\n{extra}\n"
    )
}

#[test]
fn examples_parse_and_bind_the_native_diagnostics_model_to_its_station() {
    let parsed =
        parse_configuration(include_str!("../examples/diagnostics-1.6-config.toml")).unwrap();
    let options = parsed.stations[0].diagnostics16.as_ref().unwrap();
    assert!(options.legacy && options.security_log);
    assert_eq!(
        (options.cancel_policy, options.get_log_failure_status),
        (LogCancelPolicy::Cancel, LogFailureStatus::UploadFailure)
    );
    let client = parsed.stations[0].client_config();
    assert_eq!(client.diagnostics16.as_ref().unwrap().0, "alpha");
    let scenario = parse_scenario(include_str!("../examples/diagnostics-1.6.toml")).unwrap();
    assert!(
        scenario
            .steps
            .iter()
            .any(|step| matches!(step.action, ActionKind::AwaitDiagnostics))
    );
    // CiString20: the example boot model must be accepted by a real station library.
    let boot = scenario
        .steps
        .iter()
        .find_map(|step| step.payload.as_ref()?.get("chargePointModel")?.as_str())
        .unwrap();
    assert!(boot.len() <= 20);
}

#[test]
fn invalid_diagnostics_station_configuration_fails_setup() {
    for (edition, diagnostics) in [
        ("2.0.1", "legacy=true"),
        ("1.6", "legacy=false"),
        (
            "1.6",
            "legacy=false\nsecurity_log=true\nno_diagnostics=true",
        ),
        ("1.6", "reject_get_log=true"),
        ("1.6", "maximum_bytes=0"),
        ("1.6", "maximum_bytes=10\ndiagnostics_bytes=11"),
        ("1.6", "upload_failures=17"),
        ("1.6", "security_log=true\ncancel_policy='ignore'"),
        ("1.6", "security_log=true\nget_log_failure_status='Idle'"),
        ("1.6", "unknown=true"),
    ] {
        let failure = parse_configuration(&configuration(edition, diagnostics)).unwrap_err();
        assert!(
            matches!(
                failure.code,
                "invalid_diagnostics_configuration" | "invalid_configuration_toml"
            ),
            "{edition} {diagnostics}: {}",
            failure.code
        );
    }
    assert!(parse_configuration(&configuration("1.6", "legacy=true")).is_ok());
    assert!(
        parse_configuration(&configuration(
            "1.6",
            "legacy=false\nsecurity_log=true\ncancel_policy='reject'"
        ))
        .is_ok()
    );
}

#[test]
fn diagnostics_actions_accept_only_safe_expectations() {
    let awaited = parse_scenario(&step(
        "await_diagnostics",
        "expect_response={lastStatus='Uploaded',uploads=1}",
    ))
    .unwrap();
    assert_eq!(awaited.steps[0].action.name(), "await_diagnostics");
    assert_eq!(awaited.steps[0].action.event(), "diagnostics_observed");
    assert!(
        parse_scenario(&step(
            "assert_diagnostics",
            "expect_response={active=false}"
        ))
        .is_ok()
    );
    let with_payload =
        parse_scenario(&step("assert_diagnostics", "payload={location='x'}")).unwrap_err();
    assert_eq!(with_payload.code, "invalid_action_payload");
    let with_event = parse_scenario(&step(
        "await_diagnostics",
        "expect_event='firmware_observed'",
    ))
    .unwrap_err();
    assert_eq!(with_event.code, "unsupported_expected_event");
}
