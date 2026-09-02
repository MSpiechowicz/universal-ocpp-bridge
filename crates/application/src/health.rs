use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use crate::{
    DatabaseHealth, DatabaseHealthState, RuntimeResourceBudget, RuntimeResourceLimits,
    RuntimeResourceSnapshot, StorageAdmissionState, StorageRetentionStatus, TargetHealth,
    TargetHealthState,
};

const LATENCY_BUCKET_UPPER_MS: [u64; 10] = [1, 5, 10, 25, 50, 100, 250, 500, 1_000, 5_000];

/// Release-supervisor view of the safety-critical local core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadinessState {
    Ready,
    NotReady,
}

/// Liveness of the station/application coordination loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreLoopState {
    Starting,
    Ready,
    Failed,
}

/// Authoritative storage safety and new-session admission state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageHealthState {
    Starting,
    Safe,
    CapacityProtected,
    Failed,
}

/// Independently supervised non-core component family.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ComponentKind {
    Target,
    ExternalBroker,
    ExternalClient,
    ExternalExporter,
}

/// Component state that deliberately does not determine core readiness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComponentHealthState {
    Disabled,
    Ready,
    Degraded,
    Reconnecting,
    Stopped,
}

/// Sanitized component health and daemon-owned backlog accounting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentHealth {
    pub state: ComponentHealthState,
    pub reconnects: u64,
    pub backlog_items: usize,
    pub in_flight_items: usize,
    pub active_connections: usize,
    pub reason: Option<String>,
}

/// Latency streams needed to evaluate local-response and storage targets.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LatencyClass {
    LocalResponse,
    StorageOperation,
}

/// Bounded latency summary; p95 is the upper bound of the matching fixed bucket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LatencySnapshot {
    pub samples: u64,
    pub p95_upper_bound_ms: Option<u64>,
    pub maximum_ms: u64,
    pub overflow_samples: u64,
}

/// Process families measured outside the daemon budget.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AuxiliaryProcess {
    Broker,
    ExternalDatabase,
    Browser,
    Simulator,
    Provider,
    ReleaseManager,
}

/// Resource counters for a separately measured process family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessResourceMetrics {
    pub rss_bytes: u64,
    pub cpu_time_milliseconds: u64,
}

/// Complete operator snapshot. Reference thresholds remain documentation, not pass claims.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthSnapshot {
    pub readiness: ReadinessState,
    pub core_loop: CoreLoopState,
    pub storage: StorageHealthState,
    pub accepts_new_sessions: bool,
    pub storage_retention: Option<StorageRetentionStatusView>,
    pub runtime: RuntimeResourceSnapshot,
    pub runtime_limits: RuntimeResourceLimits,
    pub local_response_latency: LatencySnapshot,
    pub storage_latency: LatencySnapshot,
    pub uncertain_commands: u64,
    pub dropped_telemetry: u64,
    pub components: BTreeMap<ComponentKind, ComponentHealth>,
    pub daemon_process: Option<ProcessResourceMetrics>,
    pub auxiliary_processes: BTreeMap<AuxiliaryProcess, ProcessResourceMetrics>,
}

/// Serializable projection of storage pressure without physical database details.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageRetentionStatusView {
    pub budget_bytes: u64,
    pub active_session_reserve_bytes: u64,
    pub used_bytes: u64,
    pub retained_critical_events: u64,
    pub retained_required_deliveries: u64,
    pub dropped_best_effort_telemetry: u64,
    pub dropped_best_effort_deliveries: u64,
    pub pruned_expired_events: u64,
    pub pruned_expired_delivery_attempts: u64,
}

impl From<StorageRetentionStatus> for StorageRetentionStatusView {
    fn from(value: StorageRetentionStatus) -> Self {
        Self {
            budget_bytes: value.budget_bytes,
            active_session_reserve_bytes: value.active_session_reserve_bytes,
            used_bytes: value.used_bytes,
            retained_critical_events: value.retained_critical_events,
            retained_required_deliveries: value.retained_required_deliveries,
            dropped_best_effort_telemetry: value.dropped_best_effort_telemetry,
            dropped_best_effort_deliveries: value.dropped_best_effort_deliveries,
            pruned_expired_events: value.pruned_expired_events,
            pruned_expired_delivery_attempts: value.pruned_expired_delivery_attempts,
        }
    }
}

#[derive(Clone, Debug)]
pub struct HealthMonitor {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    budget: RuntimeResourceBudget,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    core_loop: CoreLoopState,
    storage: StorageHealthState,
    storage_retention: Option<StorageRetentionStatusView>,
    latencies: BTreeMap<LatencyClass, LatencyHistogram>,
    uncertain_commands: u64,
    dropped_telemetry: u64,
    components: BTreeMap<ComponentKind, ComponentHealth>,
    daemon_process: Option<ProcessResourceMetrics>,
    auxiliary_processes: BTreeMap<AuxiliaryProcess, ProcessResourceMetrics>,
}

#[derive(Clone, Debug, Default)]
struct LatencyHistogram {
    buckets: [u64; 10],
    overflow: u64,
    samples: u64,
    maximum_ms: u64,
}

impl LatencyHistogram {
    fn record(&mut self, milliseconds: u64) {
        self.samples = self.samples.saturating_add(1);
        self.maximum_ms = self.maximum_ms.max(milliseconds);
        if let Some(index) = LATENCY_BUCKET_UPPER_MS
            .iter()
            .position(|upper| milliseconds <= *upper)
        {
            self.buckets[index] = self.buckets[index].saturating_add(1);
        } else {
            self.overflow = self.overflow.saturating_add(1);
        }
    }

    fn snapshot(&self) -> LatencySnapshot {
        let rank = self.samples.saturating_mul(95).div_ceil(100);
        let mut cumulative = 0_u64;
        let p95 = self.buckets.iter().enumerate().find_map(|(index, count)| {
            cumulative = cumulative.saturating_add(*count);
            (rank > 0 && cumulative >= rank).then_some(LATENCY_BUCKET_UPPER_MS[index])
        });
        LatencySnapshot {
            samples: self.samples,
            p95_upper_bound_ms: p95,
            maximum_ms: self.maximum_ms,
            overflow_samples: self.overflow,
        }
    }
}

impl HealthMonitor {
    #[must_use]
    pub fn new(budget: RuntimeResourceBudget) -> Self {
        let latencies = [
            (LatencyClass::LocalResponse, LatencyHistogram::default()),
            (LatencyClass::StorageOperation, LatencyHistogram::default()),
        ]
        .into_iter()
        .collect();
        Self {
            inner: Arc::new(Inner {
                budget,
                state: Mutex::new(State {
                    core_loop: CoreLoopState::Starting,
                    storage: StorageHealthState::Starting,
                    storage_retention: None,
                    latencies,
                    uncertain_commands: 0,
                    dropped_telemetry: 0,
                    components: BTreeMap::new(),
                    daemon_process: None,
                    auxiliary_processes: BTreeMap::new(),
                }),
            }),
        }
    }

    #[must_use]
    pub fn resources(&self) -> &RuntimeResourceBudget {
        &self.inner.budget
    }

    pub fn report_core_loop(&self, state: CoreLoopState) {
        lock(&self.inner).core_loop = state;
    }

    pub fn report_storage(
        &self,
        state: StorageHealthState,
        status: Option<StorageRetentionStatus>,
    ) {
        let mut current = lock(&self.inner);
        current.storage = state;
        current.storage_retention = status.map(Into::into);
    }

    pub fn report_storage_retention(&self, status: StorageRetentionStatus) {
        let state = if status.new_session_admission == StorageAdmissionState::Available {
            StorageHealthState::Safe
        } else {
            StorageHealthState::CapacityProtected
        };
        self.report_storage(state, Some(status));
    }

    pub fn report_component(&self, kind: ComponentKind, health: ComponentHealth) {
        lock(&self.inner).components.insert(kind, health);
    }

    /// Projects target-session health into the common component view.
    pub fn report_target(&self, health: &TargetHealth) {
        let reconnecting = health.state == TargetHealthState::Reconnecting;
        let mut state = lock(&self.inner);
        let reconnects = state.components.get(&ComponentKind::Target).map_or(
            u64::from(reconnecting),
            |previous| {
                previous.reconnects.saturating_add(u64::from(
                    reconnecting && previous.state != ComponentHealthState::Reconnecting,
                ))
            },
        );
        state.components.insert(
            ComponentKind::Target,
            ComponentHealth {
                state: match health.state {
                    TargetHealthState::Starting | TargetHealthState::Degraded => {
                        ComponentHealthState::Degraded
                    }
                    TargetHealthState::Ready => ComponentHealthState::Ready,
                    TargetHealthState::Reconnecting => ComponentHealthState::Reconnecting,
                    TargetHealthState::Stopped => ComponentHealthState::Stopped,
                },
                reconnects,
                backlog_items: health.delivery_backlog,
                in_flight_items: health.in_flight_deliveries,
                active_connections: health.active_connections,
                reason: health.reason.clone(),
            },
        );
    }

    /// Projects optional external-export health without changing local core readiness.
    pub fn report_exporter(&self, health: &DatabaseHealth) {
        let reconnecting = health.state == DatabaseHealthState::Reconnecting;
        let mut state = lock(&self.inner);
        let reconnects = state
            .components
            .get(&ComponentKind::ExternalExporter)
            .map_or(u64::from(reconnecting), |previous| {
                previous.reconnects.saturating_add(u64::from(
                    reconnecting && previous.state != ComponentHealthState::Reconnecting,
                ))
            });
        state.components.insert(
            ComponentKind::ExternalExporter,
            ComponentHealth {
                state: match health.state {
                    DatabaseHealthState::Starting | DatabaseHealthState::Degraded => {
                        ComponentHealthState::Degraded
                    }
                    DatabaseHealthState::Ready => ComponentHealthState::Ready,
                    DatabaseHealthState::Reconnecting => ComponentHealthState::Reconnecting,
                    DatabaseHealthState::Stopped => ComponentHealthState::Stopped,
                },
                reconnects,
                backlog_items: health.batch_backlog,
                in_flight_items: health.in_flight_batches,
                active_connections: health.active_connections,
                reason: health.reason.clone(),
            },
        );
    }
    pub fn record_latency(&self, class: LatencyClass, milliseconds: u64) {
        lock(&self.inner)
            .latencies
            .entry(class)
            .or_default()
            .record(milliseconds);
    }
    pub fn record_uncertain_command(&self) {
        let mut state = lock(&self.inner);
        state.uncertain_commands = state.uncertain_commands.saturating_add(1);
    }
    pub fn record_dropped_telemetry(&self, count: u64) {
        let mut state = lock(&self.inner);
        state.dropped_telemetry = state.dropped_telemetry.saturating_add(count);
    }
    pub fn report_daemon_process(&self, metrics: ProcessResourceMetrics) {
        lock(&self.inner).daemon_process = Some(metrics);
    }
    pub fn report_auxiliary_process(
        &self,
        process: AuxiliaryProcess,
        metrics: ProcessResourceMetrics,
    ) {
        lock(&self.inner)
            .auxiliary_processes
            .insert(process, metrics);
    }

    #[must_use]
    pub fn snapshot(&self) -> HealthSnapshot {
        let state = lock(&self.inner);
        let readiness = if state.core_loop == CoreLoopState::Ready
            && matches!(
                state.storage,
                StorageHealthState::Safe | StorageHealthState::CapacityProtected
            ) {
            ReadinessState::Ready
        } else {
            ReadinessState::NotReady
        };
        HealthSnapshot {
            readiness,
            core_loop: state.core_loop,
            storage: state.storage,
            accepts_new_sessions: state.core_loop == CoreLoopState::Ready
                && state.storage == StorageHealthState::Safe,
            storage_retention: state.storage_retention,
            runtime: self.inner.budget.snapshot(),
            runtime_limits: *self.inner.budget.limits(),
            local_response_latency: state.latencies[&LatencyClass::LocalResponse].snapshot(),
            storage_latency: state.latencies[&LatencyClass::StorageOperation].snapshot(),
            uncertain_commands: state.uncertain_commands,
            dropped_telemetry: state.dropped_telemetry,
            components: state.components.clone(),
            daemon_process: state.daemon_process,
            auxiliary_processes: state.auxiliary_processes.clone(),
        }
    }
}

fn lock(inner: &Inner) -> MutexGuard<'_, State> {
    inner.state.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
