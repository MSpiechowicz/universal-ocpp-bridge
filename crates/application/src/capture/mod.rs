//! Process-local capture controls. Trace storage/transport implement the separate ring contract.
#[cfg(test)]
mod tests;
mod types;
pub use types::*;

use std::{
    sync::{Arc, Mutex},
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
}

#[derive(Debug, Default)]
struct State {
    worker_running: bool,
    next_id: u64,
    next_export: u64,
    session: Option<Session>,
}

/// One shared authority per service. Construction and ordinary monitoring do not start capture.
#[derive(Clone, Debug)]
pub struct CaptureManager {
    enabled: bool,
    state: Arc<Mutex<State>>,
}

impl CaptureManager {
    /// Creates a host-configured authority; production and demo must opt in explicitly.
    #[must_use]
    pub fn new(allow_capture: bool) -> Self {
        Self {
            enabled: allow_capture,
            state: Arc::default(),
        }
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
        })
    }
}

/// Revocable sink permission, not a copy of retained trace memory. Sinks must check each read.
#[derive(Debug)]
pub struct CaptureLease {
    manager: CaptureManager,
    id: u64,
    export_id: Option<u64>,
}

impl CaptureLease {
    /// Checks session lifetime and exact record selection for every streamed/exported record.
    #[must_use]
    pub fn permits(&self, record: &CaptureFilter) -> bool {
        let mut state = lock(&self.manager.state);
        reap(&mut state, Instant::now());
        state.session.as_ref().is_some_and(|s| {
            s.status.id == self.id
                && s.status.filter.includes(record)
                && self
                    .export_id
                    .map_or(!s.stopped, |id| s.exports.iter().any(|(v, _)| *v == id))
        })
    }
}

impl Drop for CaptureLease {
    fn drop(&mut self) {
        let mut state = lock(&self.manager.state);
        if let Some(session) = state.session.as_mut().filter(|s| s.status.id == self.id) {
            if let Some(id) = self.export_id {
                session.exports.retain(|(v, _)| *v != id);
            } else {
                session.subscribers = session.subscribers.saturating_sub(1);
            }
        }
        reap(&mut state, Instant::now());
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
