use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::timeout;

use super::execution::execute_action;
use super::fault::complete_heartbeat_pair_out_of_order;
use super::scheduling::{StepWork, deterministic_jitter, fault_selected, validate_and_group_steps};
use super::{
    DiagnosticCounts, FailureCategory, FaultKind, RunFailure, RunReport, ScenarioDefinition,
    SimulatorConfiguration, StationDefinition, StationState, StepDefinition,
};
use crate::{ClientDiagnostics, ProtocolClient, SimulatorClientConfig, SimulatorProtocolClient};

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
        let mut report = RunReport::new(seed);
        let grouped = match validate_and_group_steps(configuration, scenario) {
            Ok(grouped) => grouped,
            Err((failure, rejected_steps)) => {
                report.diagnostics.rejected_steps = rejected_steps;
                report.finish_failure(failure);
                return report;
            }
        };
        report.push("run_started", "running", None, None, None, None);

        let (stop, stop_token) = cancellation_pair();
        let mut workers = JoinSet::new();
        for station in &configuration.stations {
            let Some(steps) = grouped.get(&station.id) else {
                continue;
            };
            let (sender, receiver) = mpsc::channel(station.step_capacity);
            for step in steps.iter().cloned() {
                if sender.try_send(step).is_err() {
                    report.diagnostics.rejected_steps += 1;
                    report.finish_failure(RunFailure::new(
                        FailureCategory::Setup,
                        "station_step_queue_rejected",
                        "validated station work was rejected by its bounded queue",
                    ));
                    return report;
                }
            }
            drop(sender);
            workers.spawn(run_station(
                Arc::clone(&self.connector),
                Arc::clone(&self.clock),
                station.clone(),
                receiver,
                seed,
                stop_token.clone(),
            ));
        }

        let mut runs = Vec::new();
        let mut primary_failure = None;
        while !workers.is_empty() {
            tokio::select! {
                () = cancellation.cancelled(), if primary_failure.is_none() => {
                    primary_failure = Some(RunFailure::new(
                        FailureCategory::Cancelled,
                        "cancelled",
                        "scenario cancelled",
                    ));
                    stop.cancel();
                }
                joined = workers.join_next() => {
                    match joined {
                        Some(Ok(run)) => {
                            if primary_failure.is_none()
                                && let Some(failure) = run.failure.clone()
                            {
                                primary_failure = Some(failure);
                                stop.cancel();
                            }
                            runs.push(run);
                        }
                        Some(Err(_)) => {
                            if primary_failure.is_none() {
                                primary_failure = Some(RunFailure::new(
                                    FailureCategory::Setup,
                                    "station_worker_failed",
                                    "a station worker stopped unexpectedly",
                                ));
                                stop.cancel();
                            }
                        }
                        None => break,
                    }
                }
            }
        }

        let mut executions: Vec<_> = runs
            .into_iter()
            .flat_map(|run| {
                report.diagnostics.merge(run.diagnostics);
                run.executions
            })
            .collect();
        executions.sort_by_key(|execution| execution.index);
        for execution in executions {
            report_step(&mut report, &execution);
        }

        if let Some(failure) = primary_failure {
            report.finish_failure(failure);
        } else {
            report.finish_success();
        }
        report
    }
}

struct StepExecution {
    index: usize,
    step: StepDefinition,
    selected_fault: Option<FaultKind>,
    result: Result<String, RunFailure>,
}

struct StationRun {
    executions: Vec<StepExecution>,
    failure: Option<RunFailure>,
    diagnostics: DiagnosticCounts,
}

async fn run_station(
    connector: Arc<dyn ScenarioConnector>,
    clock: Arc<dyn ScenarioClock>,
    station: StationDefinition,
    mut steps: mpsc::Receiver<StepWork>,
    seed: u64,
    mut cancellation: CancellationToken,
) -> StationRun {
    let mut state = StationState::from_definition(&station);
    let mut client = None;
    let mut executions = Vec::new();
    let mut failure = None;
    let mut diagnostics = DiagnosticCounts::default();

    while let Some(work) = steps.recv().await {
        let selected_fault = work
            .step
            .fault
            .as_ref()
            .filter(|fault| fault_selected(seed, &station.id, &work.step.id, fault))
            .map(|fault| fault.kind);
        let result = {
            let execution = execute_step(
                &connector,
                &clock,
                &station,
                &work.step,
                selected_fault,
                &mut client,
                &mut state,
                &mut diagnostics,
                seed,
            );
            tokio::pin!(execution);
            tokio::select! {
                () = cancellation.cancelled() => Err(RunFailure::new(
                    FailureCategory::Cancelled,
                    "cancelled",
                    "scenario cancelled",
                )),
                result = timeout(Duration::from_millis(work.step.timeout_ms), &mut execution) => {
                    result.unwrap_or_else(|_| Err(RunFailure::new(
                        FailureCategory::Timeout,
                        "step_timeout",
                        "scenario step exceeded its wall-clock timeout",
                    )))
                }
            }
        };
        if let Err(step_failure) = &result {
            failure = Some(step_failure.clone());
        }
        executions.push(StepExecution {
            index: work.index,
            step: work.step,
            selected_fault,
            result,
        });
        if failure.is_some() {
            break;
        }
    }

    let force = failure.as_ref().is_some_and(|failure| {
        matches!(
            failure.category,
            FailureCategory::Timeout | FailureCategory::Cancelled
        )
    });
    cleanup_client(&mut client, force, &mut diagnostics).await;
    StationRun {
        executions,
        failure,
        diagnostics,
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_step(
    connector: &Arc<dyn ScenarioConnector>,
    clock: &Arc<dyn ScenarioClock>,
    station: &StationDefinition,
    step: &StepDefinition,
    selected_fault: Option<FaultKind>,
    client: &mut Option<Box<dyn ProtocolClient>>,
    state: &mut StationState,
    diagnostics: &mut DiagnosticCounts,
    seed: u64,
) -> Result<String, RunFailure> {
    let jitter = deterministic_jitter(seed, &station.id, &step.id, step.jitter_ms);
    let start_delay = step.start_delay_ms.saturating_add(jitter);
    if start_delay > 0 {
        clock.sleep(Duration::from_millis(start_delay)).await;
    }

    if matches!(selected_fault, Some(FaultKind::Disconnect)) {
        cleanup_client(client, true, diagnostics).await;
        state.connected = false;
    }
    if matches!(selected_fault, Some(FaultKind::MissingResponse)) {
        let connected = client
            .as_deref()
            .ok_or_else(|| assertion_failure("not_connected", "station is not connected"))?;
        let _ = connected.heartbeat().await;
        std::future::pending::<()>().await;
    }
    if matches!(selected_fault, Some(FaultKind::OutOfOrderResponse)) {
        let connected = client
            .as_deref()
            .ok_or_else(|| assertion_failure("not_connected", "station is not connected"))?;
        let delay = Duration::from_millis(step.fault.as_ref().map_or(0, |fault| fault.delay_ms));
        let result = complete_heartbeat_pair_out_of_order(connected, clock, delay).await?;
        step.assert_detail(&result)?;
        return Ok(result);
    }

    let result = execute_action(connector, clock, station, step, client, state).await?;
    if matches!(selected_fault, Some(FaultKind::ResponseDelay)) {
        let delay = step.fault.as_ref().map_or(0, |fault| fault.delay_ms);
        clock.sleep(Duration::from_millis(delay)).await;
    }
    step.assert_detail(&result)?;
    Ok(result)
}

async fn cleanup_client(
    client: &mut Option<Box<dyn ProtocolClient>>,
    force: bool,
    diagnostics: &mut DiagnosticCounts,
) {
    let Some(client) = client.take() else {
        return;
    };
    merge_client_diagnostics(diagnostics, client.diagnostics());
    let requires_force = force
        || !matches!(
            timeout(Duration::from_secs(2), client.shutdown()).await,
            Ok(Ok(()))
        );
    if requires_force {
        let _ = timeout(Duration::from_secs(2), client.force_shutdown()).await;
        client.abort();
    }
    merge_client_diagnostics(diagnostics, client.diagnostics());
    tokio::task::yield_now().await;
}

fn merge_client_diagnostics(counts: &mut DiagnosticCounts, client: ClientDiagnostics) {
    counts.rejected_commands = counts.rejected_commands.max(client.rejected_commands);
    counts.dropped_traces = counts.dropped_traces.max(client.dropped_traces);
}

fn report_step(report: &mut RunReport, execution: &StepExecution) {
    let step = &execution.step;
    report.push(
        "step_started",
        "running",
        Some(&step.id),
        Some(&step.station),
        Some(step.action.name()),
        None,
    );
    if let Some(fault) = execution.selected_fault {
        report.push(
            "fault_selected",
            "running",
            Some(&step.id),
            Some(&step.station),
            Some(step.action.name()),
            Some(fault.name().to_owned()),
        );
    }
    match &execution.result {
        Ok(detail) => report.push(
            "step_passed",
            "passed",
            Some(&step.id),
            Some(&step.station),
            Some(step.action.name()),
            Some(detail.clone()),
        ),
        Err(failure) => report.push(
            "step_failed",
            "failed",
            Some(&step.id),
            Some(&step.station),
            Some(step.action.name()),
            Some(failure.message.clone()),
        ),
    }
}

fn assertion_failure(code: &'static str, message: &'static str) -> RunFailure {
    RunFailure::new(FailureCategory::Assertion, code, message)
}
