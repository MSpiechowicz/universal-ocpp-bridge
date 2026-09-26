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
    pub actual_event: Option<&'static str>,
    pub failure_category: Option<super::FailureCategory>,
    pub failure_code: Option<&'static str>,
    pub detail_assertion: bool,
    pub fault_selected: Option<bool>,
    pub fault: Option<&'static str>,
    pub intervention: Option<Intervention>,
    pub eligible_controls: &'static [&'static str],
    pub effect_status: Option<&'static str>,
    pub response_delay_scope: Option<&'static str>,
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
                        actual_event: None,
                        failure_category: None,
                        failure_code: None,
                        detail_assertion: step.expect_detail.is_some(),
                        fault_selected: None,
                        fault: step.fault.as_ref().map(|fault| fault.kind.name()),
                        intervention: None,
                        eligible_controls: step.eligible_controls(),
                        effect_status: None,
                        response_delay_scope: step.response_delay_scope(),
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
                if !step.eligible_controls().contains(&fault.name()) {
                    return Err("invalid_fault_action");
                }
                if fault == FaultKind::ResponseDelay
                    && matches!(
                        step.action,
                        ActionKind::AwaitRemoteStart | ActionKind::AwaitRemoteStop
                    )
                    && delay_ms >= step.timeout_ms
                {
                    return Err("delay_exceeds_step_deadline");
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
        state.progress.eligible_controls = &[];
        state.progress.effect_status = Some("scheduled");
        state.progress.intervention = Some(intervention);
        state.step = step;
        Ok(())
    }

    /// Reserve an authored step before arming a socket handler.
    pub(super) fn prepare(&self, original: &StepDefinition) -> StepDefinition {
        let mut states = self.0.lock().expect("live run lock");
        let state = states
            .iter_mut()
            .find(|state| state.step.id == original.id)
            .expect("validated step");
        state.progress.status = "preparing";
        state.step.clone()
    }

    pub(super) fn begin(&self, original: &StepDefinition) -> StepDefinition {
        let mut states = self.0.lock().expect("live run lock");
        let state = states
            .iter_mut()
            .find(|state| state.step.id == original.id)
            .expect("validated step");
        state.progress.status = "running";
        if state.progress.intervention.is_some() {
            state.progress.effect_status = Some("in_progress");
        }
        state.step.clone()
    }

    pub(super) fn observe(&self, step_id: &str, event: Option<&'static str>, fault: Option<bool>) {
        let mut states = self.0.lock().expect("live run lock");
        let state = states
            .iter_mut()
            .find(|state| state.step.id == step_id)
            .expect("validated step");
        if let Some(event) = event {
            state.progress.actual_event = Some(event);
        }
        if let Some(fault) = fault {
            state.progress.fault_selected = Some(fault);
        }
    }

    pub(super) fn applied(&self, step_id: &str) {
        let mut states = self.0.lock().expect("live run lock");
        let state = states
            .iter_mut()
            .find(|state| state.step.id == step_id)
            .expect("validated step");
        if matches!(
            state.progress.intervention.as_ref(),
            Some(Intervention::Disconnect | Intervention::Reconnect)
        ) || (matches!(
            state.progress.intervention.as_ref(),
            Some(Intervention::Fault { .. })
        ) && state.progress.fault_selected == Some(true))
        {
            state.progress.effect_status = Some("applied");
        }
    }

    pub(super) fn finish(&self, step_id: &str, result: &Result<String, super::RunFailure>) {
        let passed = result.is_ok();
        let mut states = self.0.lock().expect("live run lock");
        let state = states
            .iter_mut()
            .find(|state| state.step.id == step_id)
            .expect("validated step");
        state.progress.status = if passed { "passed" } else { "failed" };
        state.progress.assertion_passed = Some(passed);
        if state.progress.intervention.is_some() && state.progress.effect_status != Some("applied")
        {
            state.progress.effect_status = Some(if !passed {
                "failed"
            } else if matches!(
                state.progress.intervention,
                Some(Intervention::Fault { .. })
            ) && state.progress.fault_selected == Some(false)
            {
                "not_selected"
            } else {
                "not_observed"
            });
        }
        if let Err(failure) = result {
            state.progress.failure_category = Some(failure.category);
            state.progress.failure_code = Some(failure.code);
        }
    }

    pub(crate) fn finish_pending(&self) {
        for state in self.0.lock().expect("live run lock").iter_mut() {
            if state.progress.effect_status == Some("scheduled") {
                state.progress.effect_status = Some("not_applied");
            }
        }
    }
}
