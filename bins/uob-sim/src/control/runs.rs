use super::{ControlServer, configuration::MAX_RUNS};
use crate::scenario::{CancellationHandle, LiveRun, RunReport, cancellation_pair};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use tokio::task::JoinHandle;

pub(super) struct Runs {
    pub next_id: u64,
    pub entries: BTreeMap<u64, Run>,
    pub stopping: bool,
}

pub(super) struct Run {
    pub scenario: String,
    pub seed: u64,
    pub stations: BTreeSet<String>,
    pub live: LiveRun,
    pub stop: CancellationHandle,
    pub task: Option<JoinHandle<()>>,
    pub report: Option<RunReport>,
    pub stopping: bool,
}

impl ControlServer {
    pub(super) fn start(&self, scenario_id: &str, seed: Option<u64>) -> Result<u64, &'static str> {
        let scenario = self
            .configuration
            .scenarios
            .get(scenario_id)
            .ok_or("unknown_scenario")?
            .clone();
        let seed = seed.unwrap_or(scenario.seed);
        let mut runs = self.runs.lock().expect("runs lock");
        if runs.stopping {
            return Err("server_stopping");
        }
        if runs.entries.len() >= MAX_RUNS {
            return Err("run_capacity");
        }
        let stations: BTreeSet<_> = scenario
            .steps
            .iter()
            .map(|step| step.station.clone())
            .collect();
        if runs
            .entries
            .values()
            .any(|run| run.report.is_none() && !run.stations.is_disjoint(&stations))
        {
            return Err("station_busy");
        }
        let id = runs.next_id;
        runs.next_id = id.checked_add(1).ok_or("run_id_exhausted")?;
        let (stop, cancellation) = cancellation_pair();
        let live = LiveRun::new(&scenario);
        let server = self.clone();
        let progress = live.clone();
        let task = tokio::spawn(async move {
            let mut report = server
                .runner
                .run_controlled(
                    &server.configuration.simulator,
                    &scenario,
                    seed,
                    cancellation,
                    progress,
                )
                .await;
            // Keep only finite, payload-free browser evidence after the runner completes.
            for event in &mut report.events {
                event.detail = None;
            }
            if let Some(failure) = &mut report.failure {
                failure.message.clear();
            }
            let mut runs = server.runs.lock().expect("runs lock");
            if let Some(run) = runs.entries.get_mut(&id) {
                run.report = Some(report);
            }
        });
        runs.entries.insert(
            id,
            Run {
                scenario: scenario_id.to_owned(),
                seed,
                stations,
                live,
                stop,
                task: Some(task),
                report: None,
                stopping: false,
            },
        );
        Ok(id)
    }
}

impl Run {
    pub fn status(&self, id: u64, environment: &str) -> Value {
        let status = self.report.as_ref().map_or(
            if self.stopping { "stopping" } else { "running" },
            |report| {
                if report.failure.is_some() {
                    "failed"
                } else {
                    "passed"
                }
            },
        );
        // Wire response/payload text is intentionally absent from browser evidence. The normal
        // JSONL runner remains the detailed synthetic-fixture assertion authority.
        let events: Vec<_> = self.report.iter().flat_map(|report| &report.events).map(|event| json!({
            "id": event.id, "sequence": event.sequence, "event": event.event, "status": event.status,
            "seed": event.seed, "step_id": event.step_id, "station_id": event.station_id,
            "action": event.action, "failure_category": event.failure_category, "failure_code": event.failure_code,
        })).collect();
        json!({ "run_id": id, "environment": environment, "scenario": self.scenario, "seed": self.seed,
            "status": status, "steps": self.live.snapshot(), "events": events })
    }
}
