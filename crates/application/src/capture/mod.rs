//! Process-local capture controls. Trace storage/transport implement the separate ring contract.
mod lease;
mod ring;
#[cfg(test)]
mod ring_tests;
#[cfg(test)]
mod tests;
pub use lease::CaptureLease;
mod types;
pub use ring::{MAX_TRACE_RECORD_BYTES, RetainedTrace, TraceRead, TraceWindow};
pub use types::*;

use crate::{
    DiagnosticDropReason, RuntimeResourceBudget, RuntimeResourceLimits, SanitizedDiagnostic,
};
use ring::{RingLimits, TraceMemory, TraceRing};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Default capture lifetime; extensions always require an explicit authorized request.
pub const DEFAULT_CAPTURE_DURATION: Duration = Duration::from_secs(600);
/// Maximum lifetime from each explicit start/extension.
pub const MAX_CAPTURE_DURATION: Duration = Duration::from_mins(30);
/// A stopped capture can be retained only by exports, each for at most thirty seconds.
pub const MAX_CAPTURE_EXPORT_DURATION: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct Session {
    status: CaptureStatus,
    deadline: Instant,
    stopped: bool,
    subscribers: usize,
    exports: Vec<(u64, Instant)>,
    ring: TraceRing,
    dropped_at_start: u64,
}

#[derive(Debug, Default)]
struct State {
    worker_running: bool,
    next_id: u64,
    next_export: u64,
    next_trace_sequence: u64,
    session: Option<Session>,
}

/// One shared authority per service. Construction and ordinary monitoring do not start capture.
#[derive(Clone, Debug)]
pub struct CaptureManager {
    enabled: bool,
    state: Arc<Mutex<State>>,
    dropped: Arc<AtomicU64>,
    resources: RuntimeResourceBudget,
    ring_limits: RingLimits,
    memory: Arc<TraceMemory>,
}

impl CaptureManager {
    /// Creates a host-configured authority; production and demo must opt in explicitly.
    /// # Panics
    /// Only if the compile-time default runtime resource limits are invalid.
    #[must_use]
    pub fn new(allow_capture: bool) -> Self {
        Self::with_resources(
            allow_capture,
            RuntimeResourceBudget::new(RuntimeResourceLimits::default())
                .expect("default runtime limits"),
        )
    }

    /// Uses the daemon's existing shared resource pool for all retained diagnostics.
    #[must_use]
    pub fn with_resources(allow_capture: bool, resources: RuntimeResourceBudget) -> Self {
        Self {
            enabled: allow_capture,
            state: Arc::default(),
            dropped: Arc::default(),
            ring_limits: RingLimits::for_resources(&resources),
            resources,
            memory: Arc::default(),
        }
    }

    /// Creates lower-only ring limits for constrained hosts and deterministic checks.
    /// # Errors
    /// Rejects zero limits or limits above 8 MiB and 2,000 retained records.
    pub fn with_ring_limits(
        allow_capture: bool,
        bytes: usize,
        records: usize,
    ) -> Result<Self, CaptureError> {
        let mut manager = Self::new(allow_capture);
        manager.ring_limits = RingLimits::new(bytes, records)?;
        Ok(manager)
    }

    /// Filters and admits one record without waiting for capture controls or shared resources.
    /// The formatter is invoked only for the active selection, with a process-local sequence
    /// and a flag requesting optional-detail shedding. Returning `None` records a visible drop.
    pub fn try_record(
        &self,
        filter: &CaptureFilter,
        build: impl FnOnce(u64, bool) -> Option<SanitizedDiagnostic>,
    ) -> bool {
        if !self.enabled {
            return false;
        }
        let Ok(mut state) = self.state.try_lock() else {
            self.record_drop();
            return false;
        };
        let Some(session) = state.session.as_ref().filter(|s| {
            !s.stopped && Instant::now() < s.deadline && s.status.filter.includes(filter)
        }) else {
            return false;
        };
        let shed = session.ring.pressured() || self.resources.try_diagnostic_pressure();
        let sequence = state.next_trace_sequence;
        let Some(next) = sequence.checked_add(1) else {
            self.record_drop();
            return false;
        };
        state.next_trace_sequence = next;
        let Some(record) = build(sequence, shed) else {
            self.record_drop();
            return false;
        };
        let Some(session) = state.session.as_mut() else {
            return false;
        };
        if session.ring.push(sequence, record, shed, &self.resources) {
            true
        } else {
            self.record_drop();
            false
        }
    }

    fn record_drop(&self) {
        let _ = self
            .dropped
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_add(1))
            });
        self.resources
            .record_diagnostic_drop(DiagnosticDropReason::Full);
    }

    /// Process-lifetime producer drops, including contention and failed retained admission.
    /// Reading this counter does not require an active capture or acquire the capture lock.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Starts one immutable selection after permission checks and before allocating a session.
    ///
    /// # Errors
    /// Rejects disabled, unauthorized, invalid, or concurrent captures.
    pub fn start(
        &self,
        grant: &CaptureGrant,
        filter: CaptureFilter,
        level: CaptureLevel,
        duration: Option<Duration>,
    ) -> Result<CaptureStatus, CaptureError> {
        if !self.enabled {
            return Err(CaptureError::Disabled);
        }
        if !grant.permits(CapturePermission::Capture, &filter) {
            return Err(CaptureError::Forbidden);
        }
        let duration = valid_duration(duration)?;
        if level == CaptureLevel::RedactedPayload && filter.station.is_none() {
            return Err(CaptureError::Invalid);
        }
        let mut state = lock(&self.state);
        reap(&mut state, Instant::now());
        if state.session.is_some() {
            return Err(CaptureError::Conflict);
        }
        state.next_id = state.next_id.checked_add(1).ok_or(CaptureError::Capacity)?;
        let status = CaptureStatus {
            id: state.next_id,
            filter,
            level,
            remaining: duration,
        };
        state.session = Some(Session {
            status: status.clone(),
            deadline: Instant::now() + duration,
            stopped: false,
            subscribers: 0,
            exports: vec![],
            ring: TraceRing::new(self.ring_limits, Arc::clone(&self.memory)),
            dropped_at_start: self.dropped.load(Ordering::Relaxed),
        });
        if !state.worker_running {
            let weak = Arc::downgrade(&self.state);
            state.worker_running = true;
            if std::thread::Builder::new()
                .name("capture-expiry".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(Duration::from_millis(100));
                        let Some(shared) = weak.upgrade() else {
                            break;
                        };
                        let mut state = lock(&shared);
                        reap(&mut state, Instant::now());
                        if state.session.is_none() {
                            state.worker_running = false;
                            break;
                        }
                    }
                })
                .is_err()
            {
                state.worker_running = false;
                state.session = None;
                return Err(CaptureError::Capacity);
            }
        }
        Ok(status)
    }

    /// Reads status without changing expiry or allocating a session.
    ///
    /// # Errors
    /// Rejects missing read permission, an out-of-scope selection, or inactive session.
    pub fn status(&self, grant: &CaptureGrant) -> Result<CaptureStatus, CaptureError> {
        let mut state = lock(&self.state);
        reap(&mut state, Instant::now());
        let session = state
            .session
            .as_ref()
            .filter(|s| !s.stopped)
            .ok_or(CaptureError::Gone)?;
        authorize(session, grant, CapturePermission::Read)?;
        let mut status = session.status.clone();
        status.remaining = session.deadline.saturating_duration_since(Instant::now());
        Ok(status)
    }

    /// Explicitly extends this exact session from now, never changing level or filters.
    ///
    /// # Errors
    /// Rejects invalid duration, stale ID, or missing capture permission/scope.
    pub fn extend(
        &self,
        grant: &CaptureGrant,
        id: u64,
        duration: Duration,
    ) -> Result<(), CaptureError> {
        let duration = valid_duration(Some(duration))?;
        let mut state = lock(&self.state);
        reap(&mut state, Instant::now());
        let session = active(&mut state, id)?;
        authorize(session, grant, CapturePermission::Capture)?;
        session.deadline = Instant::now() + duration;
        Ok(())
    }

    /// Stops collection immediately; only bounded exports may retain the session slot.
    ///
    /// # Errors
    /// Rejects stale ID or missing capture permission/scope.
    pub fn stop(&self, grant: &CaptureGrant, id: u64) -> Result<(), CaptureError> {
        let mut state = lock(&self.state);
        reap(&mut state, Instant::now());
        let session = active(&mut state, id)?;
        authorize(session, grant, CapturePermission::Capture)?;
        session.stopped = true;
        reap(&mut state, Instant::now());
        Ok(())
    }

    /// Expires collection and export ownership independently of browser reads.
    /// A single lazy worker also reaps every 100 ms, even with no browser requests.
    pub fn expire(&self) {
        reap(&mut lock(&self.state), Instant::now());
    }

    /// Next host timer delay for this ID. `None` means its retained resources may be released.
    #[must_use]
    pub fn wake_after(&self, id: u64) -> Option<Duration> {
        let mut state = lock(&self.state);
        reap(&mut state, Instant::now());
        let session = state.session.as_ref().filter(|s| s.status.id == id)?;
        let deadline = if session.stopped {
            session
                .exports
                .iter()
                .map(|(_, deadline)| *deadline)
                .min()?
        } else {
            session.deadline
        };
        Some(deadline.saturating_duration_since(Instant::now()))
    }

    /// Cheap filter check before diagnostic formatting. No payload may bypass central redaction.
    #[must_use]
    pub fn accepts(&self, record: &CaptureFilter, level: CaptureLevel) -> bool {
        let mut state = lock(&self.state);
        reap(&mut state, Instant::now());
        state.session.as_ref().is_some_and(|s| {
            !s.stopped
                && s.status.filter.includes(record)
                && (level == CaptureLevel::Metadata
                    || s.status.level == CaptureLevel::RedactedPayload)
        })
    }

    /// Best-effort producer check that never waits for capture control or lease ownership.
    /// Contention, poisoning, expiry and missing authorization all shed optional work.
    #[must_use]
    pub fn try_accepts(&self, record: &CaptureFilter, level: CaptureLevel) -> bool {
        let Ok(state) = self.state.try_lock() else {
            return false;
        };
        state.session.as_ref().is_some_and(|s| {
            !s.stopped
                && Instant::now() < s.deadline
                && s.status.filter.includes(record)
                && (level == CaptureLevel::Metadata
                    || s.status.level == CaptureLevel::RedactedPayload)
        })
    }

    /// Reserves one of two shared live subscriber slots, or one of two thirty-second exports.
    /// The same complete selection check protects both sinks before any trace can be read.
    ///
    /// # Errors
    /// Rejects stale IDs, missing read permission/scope, and exhausted fixed capacity.
    pub fn lease(
        &self,
        grant: &CaptureGrant,
        id: u64,
        export: bool,
    ) -> Result<CaptureLease, CaptureError> {
        let mut state = lock(&self.state);
        reap(&mut state, Instant::now());
        let export_id = state
            .next_export
            .checked_add(1)
            .ok_or(CaptureError::Capacity)?;
        let session = active(&mut state, id)?;
        authorize(session, grant, CapturePermission::Read)?;
        let filter = session.status.filter.clone();
        if export {
            if session.exports.len() >= 2 {
                return Err(CaptureError::Capacity);
            }
            session
                .exports
                .push((export_id, Instant::now() + MAX_CAPTURE_EXPORT_DURATION));
            state.next_export = export_id;
        } else {
            if session.subscribers >= 2 {
                return Err(CaptureError::Capacity);
            }
            session.subscribers += 1;
        }
        Ok(CaptureLease {
            manager: self.clone(),
            id,
            export_id: export.then_some(export_id),
            filter,
        })
    }
}

fn valid_duration(duration: Option<Duration>) -> Result<Duration, CaptureError> {
    let duration = duration.unwrap_or(DEFAULT_CAPTURE_DURATION);
    if duration.is_zero() || duration > MAX_CAPTURE_DURATION {
        Err(CaptureError::Invalid)
    } else {
        Ok(duration)
    }
}
fn active(state: &mut State, id: u64) -> Result<&mut Session, CaptureError> {
    state
        .session
        .as_mut()
        .filter(|s| s.status.id == id && !s.stopped)
        .ok_or(CaptureError::Gone)
}
fn authorize(
    session: &Session,
    grant: &CaptureGrant,
    permission: CapturePermission,
) -> Result<(), CaptureError> {
    if grant.permits(permission, &session.status.filter) {
        Ok(())
    } else {
        Err(CaptureError::Forbidden)
    }
}
fn reap(state: &mut State, now: Instant) {
    if let Some(session) = &mut state.session {
        session.stopped |= now >= session.deadline;
        session.exports.retain(|(_, deadline)| now < *deadline);
        if session.stopped && session.exports.is_empty() {
            state.session = None;
        }
    }
}

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|poisoned| {
        let mut state = poisoned.into_inner();
        state.session = None; // A poisoned capture can never authorize another read.
        state
    })
}
