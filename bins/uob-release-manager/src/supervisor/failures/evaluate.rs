use super::model::{Decision, Observation, Policy, Signal as S, State};
impl State {
    pub(super) fn evaluate(&mut self, p: Policy, o: Observation, started: u64) -> Decision {
        match o.signal {
            S::CoreReady => {
                self.core_ready = true;
                self.readiness_failures.clear();
            }
            S::StartupPending
                if !self.core_ready && o.at_seconds - started >= p.startup_seconds =>
            {
                return Decision::RollbackRequired;
            }
            S::InternalReadinessFailure if o.at_seconds - started >= p.readiness_grace_seconds => {
                if self
                    .readiness_failures
                    .last()
                    .is_none_or(|last| o.at_seconds - last >= p.readiness_interval_seconds)
                {
                    // A missed scheduled check breaks consecutiveness rather than inventing evidence.
                    if self
                        .readiness_failures
                        .last()
                        .is_some_and(|last| o.at_seconds - last > p.readiness_interval_seconds)
                    {
                        self.readiness_failures.clear();
                    }
                    self.readiness_failures.push(o.at_seconds);
                    if self.readiness_failures.len() >= p.readiness_count {
                        return Decision::RollbackRequired;
                    }
                }
            }
            S::Exit {
                desired_running: true,
                unexpected: true,
            }
            | S::Watchdog
            | S::Oom => {
                if !self.exit_recorded {
                    self.exit_recorded = true;
                    self.exits
                        .retain(|at| o.at_seconds - at <= p.exit_window_seconds);
                    self.exits.push(o.at_seconds);
                    if self.exits.len() >= p.exit_count {
                        return Decision::RollbackRequired;
                    }
                }
            }
            S::FatalInvariant {
                data_valid_and_compatible: true,
            } => return Decision::RollbackRequired,
            _ => {}
        }
        Decision::Observe
    }
}
