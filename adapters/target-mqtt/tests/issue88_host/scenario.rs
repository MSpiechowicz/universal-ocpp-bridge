use super::Host;
use std::time::Duration;
use uob_sim::scenario::{
    LiveRun, RunReport, ScenarioRunner, cancellation_pair, parse_configuration, parse_scenario,
};

pub async fn run(host: &Host, consumer: impl Future<Output = ()>) -> RunReport {
    let configuration = parse_configuration(&format!(
        r#"
schema_version = 1
station_capacity = 2
[[stations]]
id = "station-a"
endpoint = "{}"
ocpp_version = "1.6"
connectors = [1]
command_capacity = 8
step_capacity = 16
request_timeout_ms = 5000
[[stations]]
id = "station-b"
endpoint = "{}"
ocpp_version = "2.0.1"
command_capacity = 8
step_capacity = 16
request_timeout_ms = 5000
[[stations.evses]]
id = 1
connectors = [1]
"#,
        host.protocol.proxy_address, host.protocol.proxy_201_address
    ))
    .unwrap();
    // Each station runs its own steps in order. Leave its OCPP socket open
    // briefly after the physical stop so the MQTT client can retry while connected.
    let source = format!(
        "{}\n\
[[steps]]\n\
id = \"post-stop-wait-16\"\n\
station = \"station-a\"\n\
action = \"wait\"\n\
timeout_ms = 9000\n\
duration_ms = 8000\n\
expect_event = \"delay_elapsed\"\n\
\n\
[[steps]]\n\
id = \"post-stop-wait-201\"\n\
station = \"station-b\"\n\
action = \"wait\"\n\
timeout_ms = 9000\n\
duration_ms = 8000\n\
expect_event = \"delay_elapsed\"\n",
        include_str!("../../../../tests/ems-http-contract-client/remote.toml")
    );
    let scenario = parse_scenario(&source).unwrap();
    let progress = LiveRun::new(&scenario);
    let monitor = progress.clone();
    let (_, cancellation) = cancellation_pair();
    let simulator = tokio::spawn(async move {
        ScenarioRunner::default()
            .run_controlled(&configuration, &scenario, 88, cancellation, progress)
            .await
    });
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let steps = monitor.snapshot();
            if ["boot-16", "boot-201"].iter().all(|id| {
                steps
                    .iter()
                    .any(|step| step.step_id == *id && step.status == "passed")
            }) {
                break;
            }
            assert!(
                !simulator.is_finished(),
                "simulator exited before both stations booted: {steps:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both committed station boots");
    consumer.await;
    let report = tokio::time::timeout(Duration::from_secs(12), simulator)
        .await
        .expect("simulator completion deadline")
        .expect("simulator task");
    assert!(
        report.failure.is_none(),
        "simulator failure: {:?}",
        report.failure
    );
    assert_simulator_effects(&report);
    report
}

fn assert_simulator_effects(report: &RunReport) {
    for station in ["station-a", "station-b"] {
        for action in [
            "await_remote_start",
            "start_transaction",
            "await_remote_stop",
            "stop_transaction",
            "wait",
        ] {
            assert!(
                report
                    .events
                    .iter()
                    .any(|event| event.station_id.as_deref() == Some(station)
                        && event.event == "step_passed"
                        && event.action == Some(action)),
                "{station} missing {action}: {:?}",
                report.events
            );
        }
        for action in ["await_remote_start", "await_remote_stop"] {
            assert!(
                report
                    .events
                    .iter()
                    .any(|event| event.station_id.as_deref() == Some(station)
                        && event.event == "step_passed"
                        && event.action == Some(action)
                        && event
                            .detail
                            .as_deref()
                            .is_some_and(|detail| detail.contains("\"accepted\":true"))),
                "{station} did not accept {action}"
            );
        }
    }
}
