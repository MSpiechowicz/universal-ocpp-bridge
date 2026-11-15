//! Security station configuration and scenario actions are validated before any socket opens.
use uob_sim::scenario::{ActionKind, parse_configuration, parse_scenario};

const STATION: &str =
    "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://127.0.0.1:9/ocpp/alpha'\n";

fn configuration(edition: &str, security: &str) -> String {
    format!(
        "{STATION}ocpp_version='{edition}'\n[stations.security16]\nprivate_state_file='/srv/security.json'\n{security}\n"
    )
}

fn step(action: &str, extra: &str) -> String {
    format!(
        "schema_version=1\nseed=127\n[[steps]]\nid='security'\nstation='alpha'\naction='{action}'\ntimeout_ms=5000\n{extra}\n"
    )
}

#[test]
fn examples_parse_and_bind_the_native_security_model_to_its_station() {
    let parsed = parse_configuration(include_str!("../examples/security-1.6-config.toml")).unwrap();
    let options = parsed.stations[0].security16.as_ref().unwrap();
    assert!(!options.duplicate_acknowledged && !options.send_before_accepted);
    assert!(!options.extended_trigger.omit);
    let client = parsed.stations[0].client_config();
    assert_eq!(client.security16.as_ref().unwrap().0, "alpha");
    let scenario = parse_scenario(include_str!("../examples/security-1.6.toml")).unwrap();
    assert!(
        scenario
            .steps
            .iter()
            .any(|step| matches!(step.action, ActionKind::EmitSecurityEvent))
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
fn invalid_security_station_configuration_fails_setup() {
    for (edition, security) in [
        ("2.0.1", ""),
        ("1.6", "extended_trigger={reply_delay_ms=30001}"),
        ("1.6", "extended_trigger={heartbeat='maybe'}"),
        ("1.6", "extended_trigger={unknown_class='accepted'}"),
        ("1.6", "unknown=true"),
    ] {
        let failure = parse_configuration(&configuration(edition, security)).unwrap_err();
        assert!(
            matches!(
                failure.code,
                "invalid_security_configuration" | "invalid_configuration_toml"
            ),
            "{edition} {security}: {}",
            failure.code
        );
    }
    assert!(
        parse_configuration(&configuration(
            "1.6",
            "duplicate_acknowledged=true\nextended_trigger={omit=true,reply_delay_ms=30000}"
        ))
        .is_ok()
    );
}

#[test]
fn emit_security_event_enforces_the_pinned_schema_bounds_at_setup() {
    let emit =
        |payload: &str| parse_scenario(&step("emit_security_event", &format!("payload={payload}")));
    let ok = emit("{type='StartupOfTheDevice',timestamp='2026-10-01T12:00:00Z',techInfo='boot'}")
        .unwrap();
    assert_eq!(ok.steps[0].action.name(), "emit_security_event");
    assert_eq!(ok.steps[0].action.event(), "security_event_queued");
    assert!(emit("{type='ResetOrReboot'}").is_ok());
    let long_type = "t".repeat(51);
    let long_info = "i".repeat(256);
    for (payload, code) in [
        (
            format!("{{type='{long_type}'}}"),
            "security_event_type_too_long",
        ),
        (
            format!("{{type='x',techInfo='{long_info}'}}"),
            "security_event_tech_info_too_long",
        ),
        (
            "{type='x',timestamp='yesterday'}".to_owned(),
            "security_event_timestamp_invalid",
        ),
        (
            "{timestamp='2026-10-01T12:00:00Z'}".to_owned(),
            "invalid_security_payload",
        ),
        ("{type=5}".to_owned(), "invalid_security_payload"),
        (
            "{type='x',extra='y'}".to_owned(),
            "invalid_security_payload",
        ),
    ] {
        assert_eq!(emit(&payload).unwrap_err().code, code, "{payload}");
    }
    let without = parse_scenario(&step("emit_security_event", "")).unwrap_err();
    assert_eq!(without.code, "invalid_action_payload");
}

#[test]
fn security_assertions_accept_only_safe_expectations() {
    let awaited = parse_scenario(&step(
        "await_security",
        "expect_response={pendingEvents=0,eventsAcknowledged=1}",
    ))
    .unwrap();
    assert_eq!(awaited.steps[0].action.event(), "security_observed");
    assert!(
        parse_scenario(&step(
            "assert_security",
            "expect_response={duplicatesSent=1}"
        ))
        .is_ok()
    );
    let with_payload = parse_scenario(&step("assert_security", "payload={type='x'}")).unwrap_err();
    assert_eq!(with_payload.code, "invalid_action_payload");
    let with_event =
        parse_scenario(&step("await_security", "expect_event='firmware_observed'")).unwrap_err();
    assert_eq!(with_event.code, "unsupported_expected_event");
}
