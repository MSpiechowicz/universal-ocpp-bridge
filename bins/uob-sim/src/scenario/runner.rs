use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::timeout;

use super::{
    ActionKind, FailureCategory, RunFailure, RunReport, ScenarioDefinition, SimulatorConfiguration,
    StationDefinition, StepDefinition,
};
use crate::{ProtocolClient, SimulatorClientConfig, SimulatorProtocolClient};

pub type ConnectFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<Box<dyn ProtocolClient>, crate::SimulatorClientError>>
            + Send
            + 'a,
    >,
>;

pub trait ScenarioConnector: Send + Sync {
    fn connect(&self, configuration: SimulatorClientConfig) -> ConnectFuture<'_>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RealConnector;

impl ScenarioConnector for RealConnector {
    fn connect(&self, configuration: SimulatorClientConfig) -> ConnectFuture<'_> {
        Box::pin(async move {
            SimulatorProtocolClient::connect(configuration)
                .await
                .map(|client| Box::new(client) as Box<dyn ProtocolClient>)
        })
    }
}

pub type SleepFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

pub trait ScenarioClock: Send + Sync {
    fn sleep(&self, duration: Duration) -> SleepFuture<'_>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RealClock;

impl ScenarioClock for RealClock {
    fn sleep(&self, duration: Duration) -> SleepFuture<'_> {
        Box::pin(tokio::time::sleep(duration))
    }
}

#[derive(Clone, Debug)]
pub struct CancellationHandle {
    sender: watch::Sender<bool>,
}

#[derive(Clone, Debug)]
pub struct CancellationToken {
    receiver: watch::Receiver<bool>,
}

#[must_use]
pub fn cancellation_pair() -> (CancellationHandle, CancellationToken) {
    let (sender, receiver) = watch::channel(false);
    (
        CancellationHandle { sender },
        CancellationToken { receiver },
    )
}

impl CancellationHandle {
    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }
}

impl CancellationToken {
    fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    async fn cancelled(&mut self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            if self.receiver.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

pub struct ScenarioRunner {
    connector: Arc<dyn ScenarioConnector>,
    clock: Arc<dyn ScenarioClock>,
}

impl Default for ScenarioRunner {
    fn default() -> Self {
        Self::new(Arc::new(RealConnector), Arc::new(RealClock))
    }
}

impl ScenarioRunner {
    #[must_use]
    pub fn new(connector: Arc<dyn ScenarioConnector>, clock: Arc<dyn ScenarioClock>) -> Self {
        Self { connector, clock }
    }

    pub async fn run(
        &self,
        configuration: &SimulatorConfiguration,
        scenario: &ScenarioDefinition,
        seed: u64,
        mut cancellation: CancellationToken,
    ) -> RunReport {
        if let Err(failure) = validate_station_references(configuration, scenario) {
            return RunReport::failed(seed, failure);
        }

        let stations: HashMap<_, _> = configuration
            .stations
            .iter()
            .map(|station| (station.id.as_str(), station))
            .collect();
        let mut clients: HashMap<String, Box<dyn ProtocolClient>> = HashMap::new();
        let mut report = RunReport::new(seed);
        report.push("run_started", "running", None, None, None, None);

        for step in &scenario.steps {
            report.push(
                "step_started",
                "running",
                Some(&step.id),
                Some(&step.station),
                Some(step.action.name()),
                None,
            );
            let station = stations[step.station.as_str()];
            let duration = Duration::from_millis(step.timeout_ms);
            let result = {
                let execution = self.execute_step(step, station, &mut clients);
                tokio::pin!(execution);
                tokio::select! {
                    () = cancellation.cancelled() => Err(RunFailure::new(
                        FailureCategory::Cancelled,
                        "cancelled",
                        "scenario cancelled",
                    )),
                    result = timeout(duration, &mut execution) => match result {
                        Ok(result) => result,
                        Err(_) => Err(RunFailure::new(
                            FailureCategory::Timeout,
                            "step_timeout",
                            "scenario step exceeded its wall-clock timeout",
                        )),
                    },
                }
            };

            match result {
                Ok(detail) => report.push(
                    "step_passed",
                    "passed",
                    Some(&step.id),
                    Some(&step.station),
                    Some(step.action.name()),
                    Some(detail),
                ),
                Err(failure) => {
                    report.push(
                        "step_failed",
                        "failed",
                        Some(&step.id),
                        Some(&step.station),
                        Some(step.action.name()),
                        Some(failure.message.clone()),
                    );
                    let force = matches!(
                        failure.category,
                        FailureCategory::Timeout | FailureCategory::Cancelled
                    );
                    cleanup_clients(&mut clients, force).await;
                    report.finish_failure(failure);
                    return report;
                }
            }
        }

        cleanup_clients(&mut clients, false).await;
        report.push("run_passed", "passed", None, None, None, None);
        report
    }

    async fn execute_step(
        &self,
        step: &StepDefinition,
        station: &StationDefinition,
        clients: &mut HashMap<String, Box<dyn ProtocolClient>>,
    ) -> Result<String, RunFailure> {
        let detail = match step.action {
            ActionKind::Connect => {
                if clients.contains_key(&step.station) {
                    return Err(assertion_failure(
                        "already_connected",
                        "station is already connected",
                    ));
                }
                let client = self
                    .connector
                    .connect(station.client_config())
                    .await
                    .map_err(|_| {
                        RunFailure::new(
                            FailureCategory::Setup,
                            "peer_unavailable",
                            "station peer is unavailable or rejected the connection",
                        )
                    })?;
                let detail = client.version().websocket_protocol().to_owned();
                clients.insert(step.station.clone(), client);
                detail
            }
            ActionKind::Heartbeat => {
                let client = connected_client(clients, &step.station)?;
                client.heartbeat().await.map_err(|_| {
                    assertion_failure("heartbeat_failed", "Heartbeat exchange failed")
                })?
            }
            ActionKind::Wait => {
                let duration_ms = step.duration_ms.expect("validated wait duration");
                self.clock.sleep(Duration::from_millis(duration_ms)).await;
                format!("{duration_ms}ms")
            }
            ActionKind::Disconnect => {
                let Some(client) = clients.remove(&step.station) else {
                    return Err(assertion_failure(
                        "not_connected",
                        "station is not connected",
                    ));
                };
                client.shutdown().await.map_err(|_| {
                    assertion_failure("disconnect_failed", "station disconnect failed")
                })?;
                "stopped".to_owned()
            }
        };
        assert_detail(step, &detail)?;
        Ok(detail)
    }
}

fn connected_client<'a>(
    clients: &'a HashMap<String, Box<dyn ProtocolClient>>,
    station_id: &str,
) -> Result<&'a dyn ProtocolClient, RunFailure> {
    clients
        .get(station_id)
        .map(AsRef::as_ref)
        .ok_or_else(|| assertion_failure("not_connected", "station is not connected"))
}

fn assert_detail(step: &StepDefinition, actual: &str) -> Result<(), RunFailure> {
    if let Some(expected) = &step.expect_detail {
        if expected != actual {
            return Err(assertion_failure(
                "unexpected_event_detail",
                "observed event detail did not match the expected value",
            ));
        }
    }
    Ok(())
}

fn validate_station_references(
    configuration: &SimulatorConfiguration,
    scenario: &ScenarioDefinition,
) -> Result<(), RunFailure> {
    let station_ids: HashSet<_> = configuration
        .stations
        .iter()
        .map(|station| station.id.as_str())
        .collect();
    if scenario
        .steps
        .iter()
        .any(|step| !station_ids.contains(step.station.as_str()))
    {
        Err(RunFailure::new(
            FailureCategory::Setup,
            "unknown_station",
            "scenario references a station not present in the configuration",
        ))
    } else {
        Ok(())
    }
}

async fn cleanup_clients(clients: &mut HashMap<String, Box<dyn ProtocolClient>>, force: bool) {
    for (_, client) in clients.drain() {
        if force {
            let _ = timeout(Duration::from_secs(2), client.force_shutdown()).await;
            client.abort();
        } else if timeout(Duration::from_secs(2), client.shutdown())
            .await
            .is_err()
        {
            client.abort();
        }
    }
    tokio::task::yield_now().await;
}

fn assertion_failure(code: &'static str, message: &'static str) -> RunFailure {
    RunFailure::new(FailureCategory::Assertion, code, message)
}
