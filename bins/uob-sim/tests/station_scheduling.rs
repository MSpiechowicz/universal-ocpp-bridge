use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::time::timeout;
use uob_sim::scenario::{
    CancellationToken, ConnectFuture, FailureCategory, ScenarioClock, ScenarioConnector,
    ScenarioRunner, SleepFuture, StationResource, StationState, cancellation_pair,
    parse_configuration, parse_scenario,
};
use uob_sim::{ClientFuture, OcppVersion, ProtocolClient, SimulatorClientConfig};

const MIXED_CONFIGURATION: &str = r#"
schema_version = 1
station_capacity = 2

[[stations]]
id = "alpha"
endpoint = "ws://127.0.0.1:9000/alpha"
ocpp_version = "1.6"
step_capacity = 8
connectors = [1]

[[stations]]
id = "beta"
endpoint = "ws://127.0.0.1:9000/beta"
ocpp_version = "2.0.1"
step_capacity = 8

[[stations.evses]]
id = 1
connectors = [1]
"#;

#[derive(Clone)]
struct CountingConnector {
    connections: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
}

impl ScenarioConnector for CountingConnector {
    fn connect(&self, configuration: SimulatorClientConfig) -> ConnectFuture<'_> {
        self.connections.fetch_add(1, Ordering::SeqCst);
        let shutdowns = Arc::clone(&self.shutdowns);
        Box::pin(async move {
            Ok(Box::new(FakeClient {
                version: configuration.version,
                shutdowns,
            }) as Box<dyn ProtocolClient>)
        })
    }
}

struct FakeClient {
    version: OcppVersion,
    shutdowns: Arc<AtomicUsize>,
}

impl ProtocolClient for FakeClient {
    fn version(&self) -> OcppVersion {
        self.version
    }

    fn heartbeat(&self) -> ClientFuture<'_, String> {
        Box::pin(async { Ok("2026-09-01T00:00:00Z".to_owned()) })
    }

    fn shutdown(&self) -> ClientFuture<'_, ()> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }

    fn force_shutdown(&self) -> ClientFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn abort(&self) {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
    }

    fn traces(&self) -> Vec<uob_sim::TraceEvent> {
        Vec::new()
    }
}

struct LongWaitBlocks;

impl ScenarioClock for LongWaitBlocks {
    fn sleep(&self, duration: Duration) -> SleepFuture<'_> {
        if duration == Duration::from_secs(10) {
            Box::pin(pending())
        } else {
            Box::pin(async {})
        }
    }
}

fn runner(
    clock: Arc<dyn ScenarioClock>,
    connections: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
) -> ScenarioRunner {
    ScenarioRunner::new(
        Arc::new(CountingConnector {
            connections,
            shutdowns,
        }),
        clock,
    )
}

fn token() -> CancellationToken {
    cancellation_pair().1
}

#[tokio::test]
async fn mixed_version_station_progress_is_independent_of_a_delayed_peer() {
    let configuration = parse_configuration(MIXED_CONFIGURATION).unwrap();
    let scenario = parse_scenario(
        r#"
schema_version = 1
seed = 24
[[steps]]
id = "alpha-connect"
station = "alpha"
action = "connect"
timeout_ms = 100
[[steps]]
id = "alpha-delay"
station = "alpha"
action = "wait"
timeout_ms = 40
duration_ms = 10000
[[steps]]
id = "beta-connect"
station = "beta"
action = "connect"
timeout_ms = 100
[[steps]]
id = "beta-heartbeat"
station = "beta"
action = "heartbeat"
timeout_ms = 100
[[steps]]
id = "beta-disconnect"
station = "beta"
action = "disconnect"
timeout_ms = 100
"#,
    )
    .unwrap();
    let report = runner(
        Arc::new(LongWaitBlocks),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    )
    .run(&configuration, &scenario, scenario.seed, token())
    .await;

    assert_eq!(report.failure.unwrap().category, FailureCategory::Timeout);
    assert!(report.events.iter().any(|event| {
        event.event == "step_passed" && event.step_id.as_deref() == Some("beta-heartbeat")
    }));
    assert!(report.events.iter().any(|event| {
        event.event == "step_passed"
            && event.step_id.as_deref() == Some("beta-connect")
            && event.detail.as_deref() == Some("ocpp2.0.1")
    }));
}

#[test]
fn equal_resource_numbers_and_transactions_remain_station_local() {
    let configuration = parse_configuration(MIXED_CONFIGURATION).unwrap();
    let mut alpha = StationState::from_definition(&configuration.stations[0]);
    let mut beta = StationState::from_definition(&configuration.stations[1]);
    let connector = StationResource::Connector { connector_id: 1 };
    let evse_connector = StationResource::EvseConnector {
        evse_id: 1,
        connector_id: 1,
    };

    alpha.start_transaction(connector, "alpha-tx").unwrap();
    beta.start_transaction(evse_connector, "beta-tx").unwrap();

    assert_eq!(
        alpha.resource(connector).unwrap().transaction_id.as_deref(),
        Some("alpha-tx")
    );
    assert_eq!(
        beta.resource(evse_connector)
            .unwrap()
            .transaction_id
            .as_deref(),
        Some("beta-tx")
    );
    assert!(alpha.resource(evse_connector).is_none());
    assert!(beta.resource(connector).is_none());
    assert_eq!(alpha.stop_transaction(connector).unwrap(), "alpha-tx");
    assert_eq!(beta.stop_transaction(evse_connector).unwrap(), "beta-tx");
}

#[tokio::test]
async fn cancellation_closes_every_station_connection_and_worker() {
    let configuration = parse_configuration(MIXED_CONFIGURATION).unwrap();
    let scenario = parse_scenario(&two_station_wait_scenario()).unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let runner = runner(
        Arc::new(LongWaitBlocks),
        Arc::clone(&connections),
        Arc::clone(&shutdowns),
    );
    let (handle, token) = cancellation_pair();
    let run = runner.run(&configuration, &scenario, 24, token);
    let cancel = async {
        timeout(Duration::from_secs(1), async {
            while connections.load(Ordering::SeqCst) != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        handle.cancel();
    };
    let (report, ()) = tokio::join!(run, cancel);

    assert_eq!(report.failure.unwrap().category, FailureCategory::Cancelled);
    assert_eq!(connections.load(Ordering::SeqCst), 2);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn bounded_station_queue_rejects_excess_work_with_diagnostics() {
    let configuration = parse_configuration(&MIXED_CONFIGURATION.replace(
        "step_capacity = 8\nconnectors",
        "step_capacity = 1\nconnectors",
    ))
    .unwrap();
    let scenario = parse_scenario(
        r#"
schema_version = 1
seed = 24
[[steps]]
id = "first"
station = "alpha"
action = "wait"
timeout_ms = 100
duration_ms = 1
[[steps]]
id = "second"
station = "alpha"
action = "wait"
timeout_ms = 100
duration_ms = 1
"#,
    )
    .unwrap();
    let report = runner(
        Arc::new(LongWaitBlocks),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    )
    .run(&configuration, &scenario, 24, token())
    .await;

    assert_eq!(
        report.failure.as_ref().unwrap().code,
        "station_step_capacity_exceeded"
    );
    assert_eq!(report.diagnostics.rejected_steps, 1);
    assert_eq!(
        report.events.last().unwrap().diagnostics,
        Some(report.diagnostics)
    );
}

#[test]
fn every_controlled_response_fault_is_accepted_by_the_scenario_schema() {
    for (kind, delay) in [
        ("disconnect", 0),
        ("response_delay", 5),
        ("missing_response", 0),
        ("out_of_order_response", 5),
    ] {
        let scenario = format!(
            r#"
schema_version = 1
seed = 24
[[steps]]
id = "fault"
station = "alpha"
action = "heartbeat"
timeout_ms = 100
jitter_ms = 3
[steps.fault]
kind = "{kind}"
probability_percent = 50
delay_ms = {delay}
"#
        );
        parse_scenario(&scenario).unwrap();
    }
}

fn two_station_wait_scenario() -> String {
    r#"
schema_version = 1
seed = 24
[[steps]]
id = "alpha-connect"
station = "alpha"
action = "connect"
timeout_ms = 100
[[steps]]
id = "alpha-wait"
station = "alpha"
action = "wait"
timeout_ms = 5000
duration_ms = 10000
[[steps]]
id = "beta-connect"
station = "beta"
action = "connect"
timeout_ms = 100
[[steps]]
id = "beta-wait"
station = "beta"
action = "wait"
timeout_ms = 5000
duration_ms = 10000
"#
    .to_owned()
}
