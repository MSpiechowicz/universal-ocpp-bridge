//! 2.0.1 log station configuration and scenario actions are validated before any socket opens.
use uob_sim::diagnostics201::{LogCancelPolicy201, LogFailureStatus201};
use uob_sim::scenario::{ActionKind, parse_configuration, parse_scenario};

const STATION: &str =
    "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://127.0.0.1:9/ocpp/alpha'\n";

fn configuration(edition: &str, diagnostics: &str) -> String {
    format!(
        "{STATION}ocpp_version='{edition}'\n[stations.diagnostics201]\nprivate_state_file='/srv/diagnostics.json'\n{diagnostics}\n"
    )
}

fn step(action: &str, extra: &str) -> String {
    format!(
        "schema_version=1\nseed=125\n[[steps]]\nid='diagnostics'\nstation='alpha'\naction='{action}'\ntimeout_ms=5000\n{extra}\n"
    )
}

#[test]
fn examples_parse_and_bind_the_native_log_model_to_its_station() {
    let parsed =
        parse_configuration(include_str!("../examples/diagnostics-2.0.1-config.toml")).unwrap();
    let options = parsed.stations[0].diagnostics201.as_ref().unwrap();
    assert_eq!(
        (options.cancel_policy, options.failure_status),
        (
            LogCancelPolicy201::Cancel,
            LogFailureStatus201::UploadFailure
        )
    );
    let client = parsed.stations[0].client_config();
    assert_eq!(client.diagnostics201.as_ref().unwrap().0, "alpha");
    assert!(client.diagnostics16.is_none());
    let scenario = parse_scenario(include_str!("../examples/diagnostics-2.0.1.toml")).unwrap();
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
        .find_map(|step| {
            step.payload
                .as_ref()?
                .pointer("/chargingStation/model")?
                .as_str()
        })
        .unwrap();
    assert!(boot.len() <= 20);
}

#[test]
fn invalid_log_station_configuration_fails_setup() {
    for (edition, diagnostics) in [
        ("1.6", ""),
        ("2.0.1", "maximum_bytes=0"),
        ("2.0.1", "maximum_bytes=10\ndiagnostics_bytes=11"),
        ("2.0.1", "upload_failures=17"),
        ("2.0.1", "reject_get_log=true\nupload_failures=1"),
        ("2.0.1", "cancel_policy='ignore'"),
        ("2.0.1", "failure_status='Idle'"),
        ("2.0.1", "legacy=true"),
        ("2.0.1", "unknown=true"),
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
    assert!(parse_configuration(&configuration("2.0.1", "")).is_ok());
    assert!(
        parse_configuration(&configuration(
            "2.0.1",
            "cancel_policy='reject'\nfailure_status='BadMessage'"
        ))
        .is_ok()
    );
}

#[test]
fn diagnostics_actions_accept_only_safe_expectations_for_the_log_station() {
    let awaited = parse_scenario(&step(
        "await_diagnostics",
        "expect_response={lastStatus='Uploaded',uploads=1,requestId=3}",
    ))
    .unwrap();
    assert_eq!(awaited.steps[0].action.name(), "await_diagnostics");
    assert!(
        parse_scenario(&step(
            "assert_diagnostics",
            "expect_response={active=false,cancelled=[1]}"
        ))
        .is_ok()
    );
    let with_payload =
        parse_scenario(&step("assert_diagnostics", "payload={remoteLocation='x'}")).unwrap_err();
    assert_eq!(with_payload.code, "invalid_action_payload");
}
