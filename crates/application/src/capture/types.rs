use std::time::Duration;
use uob_contracts::{BridgeId, StationId, TargetInstanceId};

/// Separately granted diagnostic operations; neither grants charging control.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapturePermission {
    /// Inspect an authorized capture, including streaming and export.
    Read,
    /// Explicitly start, extend, or stop a capture.
    Capture,
}

/// Immutable diagnostic scope supplied by authenticated credential configuration.
#[derive(Clone, Debug)]
pub struct CaptureGrant {
    pub(crate) bridge: BridgeId,
    pub(crate) permissions: Vec<CapturePermission>,
    pub(crate) stations: Option<Vec<StationId>>,
    pub(crate) targets: Option<Vec<TargetInstanceId>>,
}

impl CaptureGrant {
    /// Validates a grant. `None` explicitly grants all stations/targets of this bridge;
    /// an explicit empty list is invalid. Scope lists are bounded to 128 identities.
    ///
    /// # Errors
    /// Rejects missing permissions or empty/oversized scope lists.
    pub fn new(
        bridge: BridgeId,
        permissions: Vec<CapturePermission>,
        stations: Option<Vec<StationId>>,
        targets: Option<Vec<TargetInstanceId>>,
    ) -> Result<Self, CaptureError> {
        if permissions.is_empty()
            || stations
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > 128)
            || targets
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > 128)
        {
            return Err(CaptureError::Invalid);
        }
        Ok(Self {
            bridge,
            permissions,
            stations,
            targets,
        })
    }

    pub(crate) fn permits(&self, permission: CapturePermission, filter: &CaptureFilter) -> bool {
        self.bridge == filter.bridge
            && self.permissions.contains(&permission)
            && contains(self.stations.as_deref(), filter.station.as_ref())
            && contains(self.targets.as_deref(), filter.target.as_ref())
    }
}

fn contains<T: PartialEq>(granted: Option<&[T]>, requested: Option<&T>) -> bool {
    granted.is_none_or(|values| requested.is_some_and(|v| values.contains(v)))
}

/// Immutable capture selection. Missing station is allowed only for metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureFilter {
    /// Trusted bridge installation, never inferred from an untrusted record.
    pub bridge: BridgeId,
    /// Exact station, or every station for a bridge-scoped metadata capture.
    pub station: Option<StationId>,
    /// Exact target, or every target with a bridge-wide target grant.
    pub target: Option<TargetInstanceId>,
}

impl CaptureFilter {
    pub(crate) fn includes(&self, record: &Self) -> bool {
        self.bridge == record.bridge
            && self
                .station
                .as_ref()
                .is_none_or(|v| record.station.as_ref() == Some(v))
            && self
                .target
                .as_ref()
                .is_none_or(|v| record.target.as_ref() == Some(v))
    }
}

/// No unredacted payload capture level exists.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CaptureLevel {
    /// Correlation and safe metadata only.
    #[default]
    Metadata,
    /// Payloads that have already crossed the central redaction boundary.
    RedactedPayload,
}

/// One process-local session view; IDs are not durable event cursors.
#[derive(Clone, Debug)]
pub struct CaptureStatus {
    /// Monotonically increasing within this manager/process.
    pub id: u64,
    /// Selection cannot be changed by a duplicate start or an extension.
    pub filter: CaptureFilter,
    /// Maximum disclosure level for this session.
    pub level: CaptureLevel,
    /// Time remaining on the server's monotonic clock.
    pub remaining: Duration,
}

/// Stable diagnostic control failures without sensitive source details.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureError {
    /// Explicit host enablement is absent.
    Disabled,
    /// Permission or station/target scope does not cover the entire selection.
    Forbidden,
    /// A session or bounded retained export still owns the capture slot.
    Conflict,
    /// Invalid duration, scope or payload selection.
    Invalid,
    /// Session expired, stopped, or belongs to a previous process.
    Gone,
    /// Fixed subscriber/export capacity exhausted.
    Capacity,
}
