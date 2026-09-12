use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::{ActionKind, FaultDefinition, FaultKind, ScenarioDefinition, StepDefinition};

/// An intervention replaces a not-yet-started checkpoint or changes a pending step's fault.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Intervention {
    Disconnect,
    Reconnect,
    Fault { fault: FaultKind, delay_ms: u64 },
}

#[derive(Clone, Debug, Serialize)]
pub struct StepProgress {
    pub step_id: String,
    pub station_id: String,
    pub action: &'static str,
    pub status: &'static str,
    pub expectation: Option<String>,
    pub assertion_passed: Option<bool>,
    pub fault: Option<&'static str>,
    pub intervention: Option<Intervention>,
}

struct StepState {
    step: StepDefinition,
    progress: StepProgress,
}

/// Shared finite step state. Taking a step and editing it use the same lock, so late controls fail.
#[derive(Clone)]
pub struct LiveRun(Arc<Mutex<Vec<StepState>>>);

impl LiveRun {
    #[must_use]
    pub fn new(scenario: &ScenarioDefinition) -> Self {
        Self(Arc::new(Mutex::new(
            scenario
                .steps
                .iter()
                .map(|step| StepState {
                    step: step.clone(),
                    progress: StepProgress {
                        step_id: step.id.clone(),
                        station_id: step.station.clone(),
                        action: step.action.name(),
                        status: "pending",
                        expectation: step.expect_event.clone(),
                        assertion_passed: None,
                        fault: step.fault.as_ref().map(|fault| fault.kind.name()),
                        intervention: None,
                    },
                })
                .collect(),
        )))
    }

    /// Returns bounded step evidence.
    ///
    /// # Panics
    /// Panics if an internal worker poisoned the step registry.
    #[must_use]
    pub fn snapshot(&self) -> Vec<StepProgress> {
        self.0
            .lock()
            .expect("live run lock")
            .iter()
            .map(|state| state.progress.clone())
            .collect()
    }

    /// Changes only a pending step; socket operations still execute in the ordinary runner.
    ///
    /// # Errors
    /// Rejects unknown/started steps, repeated edits, incompatible faults and invalid bounds.
    ///
    /// # Panics
    /// Panics if an internal worker poisoned the step registry.
    pub fn intervene(&self, step_id: &str, intervention: Intervention) -> Result<(), &'static str> {
        let mut states = self.0.lock().expect("live run lock");
        let state = states
            .iter_mut()
            .find(|state| state.step.id == step_id)
            .ok_or("unknown_step")?;
        if state.progress.status != "pending" || state.progress.intervention.is_some() {
            return Err("step_not_editable");
        }
        let mut step = state.step.clone();
        match intervention {
            Intervention::Disconnect | Intervention::Reconnect => {
                // Explicit wait checkpoints are the only replaceable actions. Charging fixtures
                // and their assertions can never be replaced by a browser transport control.
                if !matches!(step.action, ActionKind::Wait) {
                    return Err("checkpoint_required");
                }
                step.action = if matches!(intervention, Intervention::Disconnect) {
                    ActionKind::Disconnect
                } else {
                    ActionKind::Connect
                };
                step.duration_ms = None;
                step.expect_event = Some(step.action.event().to_owned());
                step.expect_detail = None;
            }
            Intervention::Fault { fault, delay_ms } => {
                if delay_ms > 30_000 {
                    return Err("delay_limit");
                }
                step.fault = Some(FaultDefinition {
                    kind: fault,
                    delay_ms,
                    probability_percent: 100,
                });
            }
        }
        super::model::validate_scenario(&ScenarioDefinition {
            schema_version: 1,
            seed: 0,
            steps: vec![step.clone()],
        })
        .map_err(|failure| failure.code)?;
        state.progress.action = step.action.name();
        state.progress.expectation.clone_from(&step.expect_event);
        state.progress.fault = step.fault.as_ref().map(|fault| fault.kind.name());
        state.progress.intervention = Some(intervention);
        state.step = step;
        Ok(())
    }

    pub(super) fn begin(&self, original: &StepDefinition) -> StepDefinition {
        let mut states = self.0.lock().expect("live run lock");
        let state = states
            .iter_mut()
            .find(|state| state.step.id == original.id)
            .expect("validated step");
        state.progress.status = "running";
        state.step.clone()
    }

    pub(super) fn finish(&self, step_id: &str, passed: bool) {
        let mut states = self.0.lock().expect("live run lock");
        let state = states
            .iter_mut()
            .find(|state| state.step.id == step_id)
            .expect("validated step");
        state.progress.status = if passed { "passed" } else { "failed" };
        state.progress.assertion_passed = Some(passed);
    }
}
