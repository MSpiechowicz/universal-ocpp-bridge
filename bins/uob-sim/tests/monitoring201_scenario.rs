//! 2.0.1 monitoring configuration and scenario steps are validated before any socket opens.
use uob_sim::monitoring201::{MonitoringBase201, ReportFault201, SetReplyFault201};
use uob_sim::scenario::{ActionKind, parse_configuration, parse_scenario};

const STATION: &str =
    "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://127.0.0.1:9/ocpp/alpha'\n";

const VARIABLE: &str = "[[stations.monitoring201.variables]]\ncomponent={name='Connector'}\nvariable={name='Temperature'}\nvalue='40'\nmin=0.0\nmax=100.0\nmonitor_types=['UpperThreshold','Delta']\n";

fn configuration(edition: &str, monitoring: &str) -> String {
    format!("{STATION}ocpp_version='{edition}'\n[stations.monitoring201]\n{monitoring}\n")
}

fn with_variable(monitoring: &str, extra: &str) -> String {
    format!("{}{VARIABLE}{extra}", configuration("2.0.1", monitoring))
}

fn step(action: &str, extra: &str) -> String {
    format!(
        "schema_version=1\nseed=126\n[[steps]]\nid='monitoring'\nstation='alpha'\naction='{action}'\ntimeout_ms=5000\n{extra}\n"
    )
}

#[test]
fn examples_parse_and_bind_the_monitoring_model_to_its_station() {
    let parsed =
        parse_configuration(include_str!("../examples/monitoring-2.0.1-config.toml")).unwrap();
    let options = parsed.stations[0].monitoring201.as_ref().unwrap();
    assert_eq!(
        (
            options.monitors.len(),
            options.variables.len(),
            options.report_fault,
            options.set_reply_fault
        ),
        (2, 4, ReportFault201::None, SetReplyFault201::None)
    );
    assert!(
        options
            .supported_bases
            .contains(&MonitoringBase201::HardWiredOnly)
    );
    assert!(parsed.stations[0].client_config().monitoring201.is_some());
    let scenario = parse_scenario(include_str!("../examples/monitoring-2.0.1.toml")).unwrap();
    let actions: Vec<_> = scenario
        .steps
        .iter()
        .map(|step| step.action.name())
        .collect();
    for name in [
        "await_monitoring",
        "set_monitored_value",
        "emit_monitoring_events",
        "assert_monitoring",
    ] {
        assert!(actions.contains(&name), "{name}");
    }
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
fn invalid_monitoring_configuration_fails_setup() {
    let preset = |id: i32, extra: &str| {
        with_variable(
            "",
            &format!(
                "[[stations.monitoring201.monitors]]\nid={id}\nkind='hard_wired'\ncomponent={{name='Connector'}}\nvariable={{name='Temperature'}}\ntype='UpperThreshold'\nvalue=50.0\nseverity=3\n{extra}"
            ),
        )
    };
    let duplicate_variable = format!("{}{VARIABLE}", with_variable("", ""));
    let documents = [
        configuration("1.6", ""),
        configuration("2.0.1", "items_per_message_set=0"),
        configuration("2.0.1", "monitors_per_fragment=257"),
        configuration("2.0.1", "bytes_per_message_set=10"),
        configuration("2.0.1", "initial_level=10"),
        configuration("2.0.1", "report_fault='explode'"),
        configuration("2.0.1", "supported_bases=['Nothing']"),
        configuration("2.0.1", "legacy=true"),
        duplicate_variable,
        // A monitor whose type the variable does not support, with a bad id, value or severity.
        preset(1, "").replace("'UpperThreshold'\nvalue=50.0", "'Periodic'\nvalue=50.0"),
        preset(0, ""),
        preset(1, "").replace("value=50.0", "value=500.0"),
        preset(1, "").replace("severity=3", "severity=10"),
        preset(1, "").replace(
            "variable={name='Temperature'}\ntype",
            "variable={name='Other'}\ntype",
        ),
        preset(1, "unknown=1\n"),
        with_variable("", "").replace("value='40'", "value='warm'"),
        with_variable("", "").replace("min=0.0\nmax=100.0", "min=10.0\nmax=1.0"),
        // The simulator already answers these controllers.
        with_variable("", "").replace(
            "name='Connector'}\nvariable",
            "name='MonitoringCtrlr'}\nvariable",
        ),
    ];
    for document in documents {
        let failure = parse_configuration(&document).unwrap_err();
        assert!(
            matches!(
                failure.code,
                "invalid_monitoring_configuration" | "invalid_configuration_toml"
            ),
            "{document}: {}",
            failure.code
        );
    }
    assert!(parse_configuration(&configuration("2.0.1", "")).is_ok());
    assert!(
        parse_configuration(&configuration(
            "2.0.1",
            "report_fault='skip_seq_no'\nset_reply_fault='wrong_result_count'"
        ))
        .is_ok()
    );
}

#[test]
fn monitoring_steps_accept_only_safe_expectations_and_well_formed_payloads() {
    let awaited = parse_scenario(&step(
        "await_monitoring",
        "expect_response={eventsAcknowledged=3,monitorIds=[4031]}",
    ))
    .unwrap();
    assert!(matches!(
        awaited.steps[0].action,
        ActionKind::AwaitMonitoring
    ));
    assert!(
        parse_scenario(&step(
            "assert_monitoring",
            "expect_response={level=4,lastReport={requestId=1,planned=2,sent=2,outcome='Complete'}}"
        ))
        .is_ok()
    );
    let set = "payload={component={name='Connector'},variable={name='Temperature'},value=3.5}";
    assert!(parse_scenario(&step("set_monitored_value", set)).is_ok());
    let burst = "payload={component={name='Connector'},variable={name='Temperature'},count=5,monitorId=4031}";
    assert!(parse_scenario(&step("emit_monitoring_events", burst)).is_ok());
    for (action, extra, code) in [
        (
            "assert_monitoring",
            "payload={x=1}",
            "invalid_action_payload",
        ),
        ("set_monitored_value", "", "invalid_action_payload"),
        (
            "set_monitored_value",
            "payload={component={name='C'},variable={name='V'}}",
            "invalid_monitoring_payload",
        ),
        (
            "set_monitored_value",
            "payload={component={name='C'},variable={name='V'},value=true}",
            "invalid_monitoring_payload",
        ),
        (
            "set_monitored_value",
            "payload={component={},variable={name='V'},value=1}",
            "invalid_monitoring_payload",
        ),
        (
            "emit_monitoring_events",
            "payload={component={name='C'},variable={name='V'},count=0}",
            "invalid_monitoring_payload",
        ),
        (
            "emit_monitoring_events",
            "payload={component={name='C'},variable={name='V'},count=1001}",
            "invalid_monitoring_payload",
        ),
        (
            "emit_monitoring_events",
            "payload={component={name='C'},variable={name='V'},count=1,extra=1}",
            "invalid_monitoring_payload",
        ),
        (
            "set_monitored_value",
            "payload={component={name='C'},variable={name='V'},value=1}\nexpect_response={x=1}",
            "invalid_expected_response",
        ),
    ] {
        let failure = parse_scenario(&step(action, extra)).unwrap_err();
        assert_eq!(failure.code, code, "{action} {extra}");
    }
}
