use std::time::Duration;

use tokio::time::sleep;
use uob_application::DatabaseHealthState;

use super::{SharedHealth, ports::StopSignal, update};

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum CircuitState {
    #[default]
    Closed,
    Open,
    HalfOpen,
}

pub(super) struct RetryPolicy {
    failures: u32,
    jitter_state: u64,
    circuit: CircuitState,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        let clock_seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                elapsed
                    .as_secs()
                    .wrapping_mul(1_000_000_000)
                    .wrapping_add(u64::from(elapsed.subsec_nanos()))
            });
        Self {
            failures: 0,
            jitter_state: clock_seed ^ u64::from(std::process::id()),
            circuit: CircuitState::Closed,
        }
    }
}

impl RetryPolicy {
    pub(super) fn reset(&mut self) {
        self.failures = 0;
        self.circuit = CircuitState::Closed;
    }

    pub(super) fn half_open(&mut self) {
        if self.circuit == CircuitState::Open {
            // The single supervisor performs exactly one probe, never concurrent probes.
            self.circuit = CircuitState::HalfOpen;
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.circuit == CircuitState::Open
    }

    pub(super) fn next_delay(&mut self) -> Duration {
        self.failures = self.failures.saturating_add(1);
        if self.failures >= 3 {
            self.circuit = CircuitState::Open;
        }
        // Process-specific jitter avoids a thundering herd; retries stay on this worker.
        self.jitter_state = self
            .jitter_state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let capped = 100_u64
            .saturating_mul(1_u64 << self.failures.min(8))
            .min(30_000);
        let jitter = 75 + (self.jitter_state >> 32) % 26;
        let delay = capped * jitter / 100;
        if self.is_open() {
            Duration::from_millis(delay.max(2_000))
        } else {
            Duration::from_millis(delay)
        }
    }
}
pub(super) async fn retry_after_failure(
    retry: &mut RetryPolicy,
    stop: &StopSignal,
    health: &SharedHealth,
) -> bool {
    let delay = retry.next_delay();
    update(health, |state| {
        state.retry_count += 1;
        state.provider.state = if retry.is_open() {
            DatabaseHealthState::Degraded
        } else {
            DatabaseHealthState::Reconnecting
        };
        if retry.is_open() {
            state.provider.reason = Some("provider.circuit_open".into());
        }
    });
    tokio::select! {
        () = stop.wait() => true,
        () = sleep(delay) => {
            retry.half_open();
            false
        }
    }
}
