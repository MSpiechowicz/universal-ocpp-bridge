#![allow(clippy::result_large_err)]

use std::future::pending;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;
use uob_sim::scenario::{
    CancellationToken, ConnectFuture, FailureCategory, ScenarioClock, ScenarioConnector,
    ScenarioRunner, SleepFuture, cancellation_pair, parse_configuration, parse_scenario,
};
use uob_sim::{ClientFuture, OcppVersion, ProtocolClient, SimulatorClientConfig};

const CONFIGURATION: &str = r#"
schema_version = 1

[[stations]]
id = "alpha"
endpoint = "ws://127.0.0.1:9000/alpha"
ocpp_version = "1.6"
request_timeout_ms = 100
command_capacity = 2
trace_capacity = 4
"#;

const SCENARIO: &str = r#"
schema_version = 1
seed = 42

[[steps]]
id = "connect"
station = "alpha"
action = "connect"
timeout_ms = 100
expect_event = "connected"
expect_detail = "ocpp1.6"

[[steps]]
id = "heartbeat"
station = "alpha"
action = "heartbeat"
timeout_ms = 100
expect_message = "Heartbeat"
expect_event = "heartbeat_result"
expect_detail = "2026-09-01T00:00:00Z"

[[steps]]
id = "wait"
station = "alpha"
action = "wait"
timeout_ms = 100
duration_ms = 25
expect_event = "delay_elapsed"
expect_detail = "25ms"

[[steps]]
id = "disconnect"
station = "alpha"
action = "disconnect"
timeout_ms = 100
expect_event = "disconnected"
expect_detail = "stopped"
"#;

#[derive(Clone)]
struct FakeConnector {
    shutdowns: Arc<AtomicUsize>,
}

impl ScenarioConnector for FakeConnector {
    fn connect(&self, configuration: SimulatorClientConfig) -> ConnectFuture<'_> {
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

#[derive(Default)]
struct RecordingClock {
    sleeps: Mutex<Vec<Duration>>,
}

impl ScenarioClock for RecordingClock {
    fn sleep(&self, duration: Duration) -> SleepFuture<'_> {
        self.sleeps.lock().unwrap().push(duration);
        Box::pin(async {})
    }
}

struct BlockingClock;

impl ScenarioClock for BlockingClock {
    fn sleep(&self, _duration: Duration) -> SleepFuture<'_> {
        Box::pin(pending())
    }
}

fn runner(clock: Arc<dyn ScenarioClock>, shutdowns: Arc<AtomicUsize>) -> ScenarioRunner {
    ScenarioRunner::new(Arc::new(FakeConnector { shutdowns }), clock)
}

fn token() -> CancellationToken {
    cancellation_pair().1
}

#[tokio::test]
async fn identical_seed_produces_identical_logical_events_and_ids() {
    let configuration = parse_configuration(CONFIGURATION).unwrap();
    let scenario = parse_scenario(SCENARIO).unwrap();
    let first_clock = Arc::new(RecordingClock::default());
    let first = runner(first_clock.clone(), Arc::new(AtomicUsize::new(0)))
        .run(&configuration, &scenario, scenario.seed, token())
        .await;
    let second = runner(
        Arc::new(RecordingClock::default()),
        Arc::new(AtomicUsize::new(0)),
    )
    .run(&configuration, &scenario, scenario.seed, token())
    .await;

    assert_eq!(first.events, second.events);
    assert!(first.failure.is_none());
    assert_eq!(
        *first_clock.sleeps.lock().unwrap(),
        vec![Duration::from_millis(25)]
    );
    assert!(
        first
            .events
            .iter()
            .all(|event| { event.id == format!("{:016x}-{:08x}", scenario.seed, event.sequence) })
    );
}

#[tokio::test]
async fn assertion_failure_is_structured_and_stops_connected_clients() {
    let configuration = parse_configuration(CONFIGURATION).unwrap();
    let scenario = parse_scenario(&SCENARIO.replace(
        "expect_detail = \"2026-09-01T00:00:00Z\"",
        "expect_detail = \"unexpected\"",
    ))
    .unwrap();
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let report = runner(Arc::new(RecordingClock::default()), shutdowns.clone())
        .run(&configuration, &scenario, 9, token())
        .await;

    let failure = report.failure.unwrap();
    assert_eq!(failure.category, FailureCategory::Assertion);
    assert_eq!(failure.code, "unexpected_event_detail");
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn timeout_and_cancellation_are_distinct_and_cleanup_clients() {
    let configuration = parse_configuration(CONFIGURATION).unwrap();
    let scenario = parse_scenario(&wait_scenario(10)).unwrap();
    let timeout_shutdowns = Arc::new(AtomicUsize::new(0));
    let timed_out = runner(Arc::new(BlockingClock), timeout_shutdowns.clone())
        .run(&configuration, &scenario, 1, token())
        .await;
    assert_eq!(
        timed_out.failure.unwrap().category,
        FailureCategory::Timeout
    );
    assert_eq!(timeout_shutdowns.load(Ordering::SeqCst), 1);

    let cancelled_shutdowns = Arc::new(AtomicUsize::new(0));
    let runner = runner(Arc::new(BlockingClock), cancelled_shutdowns.clone());
    let (handle, token) = cancellation_pair();
    let run = runner.run(&configuration, &scenario, 2, token);
    let cancel = async move {
        tokio::task::yield_now().await;
        handle.cancel();
    };
    let (cancelled, ()) = tokio::join!(run, cancel);
    assert_eq!(
        cancelled.failure.unwrap().category,
        FailureCategory::Cancelled
    );
    assert_eq!(cancelled_shutdowns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_force_stops_a_real_socket_request_before_returning() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let (heartbeat_seen, heartbeat_seen_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(
            tcp,
            |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
             mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                response.headers_mut().insert(
                    "Sec-WebSocket-Protocol",
                    "ocpp1.6".parse().unwrap(),
                );
                Ok(response)
            },
        )
        .await
        .unwrap();
        socket.next().await.unwrap().unwrap();
        heartbeat_seen.send(()).unwrap();
        timeout(Duration::from_secs(1), socket.next()).await.is_ok()
    });
    let configuration = parse_configuration(&configuration_with_endpoint(&endpoint)).unwrap();
    let scenario = parse_scenario(&connected_heartbeat_scenario()).unwrap();
    let (handle, token) = cancellation_pair();
    let runner = ScenarioRunner::default();
    let run = runner.run(&configuration, &scenario, 8, token);
    let cancel = async move {
        heartbeat_seen_rx.await.unwrap();
        handle.cancel();
    };
    let (report, ()) = tokio::join!(run, cancel);

    assert_eq!(report.failure.unwrap().category, FailureCategory::Cancelled);
    assert!(
        server.await.unwrap(),
        "peer did not observe socket shutdown"
    );
}

#[test]
fn invalid_documents_are_rejected_without_echoing_credentials() {
    let error =
        parse_configuration("schema_version = 1\ncredentials = \"super-secret\"\n[[stations]]\n")
            .unwrap_err();
    assert_eq!(error.category, FailureCategory::Setup);
    assert!(!error.message.contains("super-secret"));

    let error =
        parse_scenario(&SCENARIO.replace("action = \"wait\"", "action = \"script\"")).unwrap_err();
    assert_eq!(error.code, "invalid_scenario_toml");
}

fn wait_scenario(timeout_ms: u64) -> String {
    format!(
        r#"
schema_version = 1
seed = 1
[[steps]]
id = "connect"
station = "alpha"
action = "connect"
timeout_ms = 100
[[steps]]
id = "wait"
station = "alpha"
action = "wait"
timeout_ms = {timeout_ms}
duration_ms = 10000
"#
    )
}

fn configuration_with_endpoint(endpoint: &str) -> String {
    format!(
        r#"
schema_version = 1
[[stations]]
id = "alpha"
endpoint = "{endpoint}"
ocpp_version = "1.6"
request_timeout_ms = 10000
"#
    )
}

fn connected_heartbeat_scenario() -> String {
    r#"
schema_version = 1
seed = 8
[[steps]]
id = "connect"
station = "alpha"
action = "connect"
timeout_ms = 10000
[[steps]]
id = "heartbeat"
station = "alpha"
action = "heartbeat"
timeout_ms = 10000
expect_message = "Heartbeat"
expect_event = "heartbeat_result"
"#
    .to_owned()
}
