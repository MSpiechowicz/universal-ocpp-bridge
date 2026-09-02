/// Default lifetime of retained operational history.
pub const OPERATIONAL_HISTORY_RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;

/// Default logical byte budget shared by the durable journal and target outbox.
pub const DEFAULT_OPERATIONAL_STORAGE_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// Default portion of the operational budget unavailable to new charging sessions.
pub const DEFAULT_ACTIVE_SESSION_RESERVE_BYTES: u64 = 16 * 1024 * 1024;

/// Why an atomic operation needs authoritative storage capacity.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StorageWritePurpose {
    /// Routine work that must not consume the protected completion reserve.
    #[default]
    Routine,
    /// A new charging session whose admission must preserve the completion reserve.
    NewSessionStart,
    /// State required to stop or durably finish an already active charging session.
    ActiveSessionCompletion,
}

/// Stable new-session admission state derived from authoritative storage pressure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageAdmissionState {
    /// New sessions may consume the non-reserved portion of the budget.
    Available,
    /// New sessions are refused so existing sessions retain completion capacity.
    CriticalCapacityExhausted,
}

/// Retention and pressure counters exposed without leaking physical storage details.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageRetentionStatus {
    /// Configured logical journal/outbox budget.
    pub budget_bytes: u64,
    /// Capacity protected from new sessions for active-session completion.
    pub active_session_reserve_bytes: u64,
    /// Current logical bytes retained by journal, outbox, and their audit history.
    pub used_bytes: u64,
    /// Current common new-session admission decision.
    pub new_session_admission: StorageAdmissionState,
    /// Critical journal rows retained locally.
    pub retained_critical_events: u64,
    /// Critical target deliveries still awaiting a final outcome.
    pub retained_required_deliveries: u64,
    /// Best-effort telemetry rows shed under pressure since database creation.
    pub dropped_best_effort_telemetry: u64,
    /// Best-effort target deliveries shed under pressure since database creation.
    pub dropped_best_effort_deliveries: u64,
    /// Expired, safely releasable journal rows removed by retention maintenance.
    pub pruned_expired_events: u64,
    /// Expired, safely releasable delivery-attempt rows removed by maintenance.
    pub pruned_expired_delivery_attempts: u64,
}
