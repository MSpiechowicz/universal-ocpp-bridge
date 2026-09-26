use std::{error::Error, fmt};

use uob_contracts::{CanonicalResource, ResourceRef};

use super::{PageLimit, StorageError, StorageErrorCode};

/// Namespace distinguishing durable command pagination from all other cursors.
pub const COMMAND_HISTORY_CURSOR_PREFIX: &str = "uob:command:";

/// Opaque, storage-owned position in a scoped command history view.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CommandHistoryCursor(String);

impl CommandHistoryCursor {
    /// Accepts a bounded namespaced cursor without interpreting its storage position.
    ///
    /// # Errors
    ///
    /// Returns `StorageErrorCode::InvalidRequest` if the cursor exceeds 8192 bytes or lacks
    /// a nonempty position after the command-history namespace prefix.
    pub fn new(value: impl Into<String>) -> Result<Self, StorageError> {
        let value = value.into();
        if value.len() > 8192
            || value
                .strip_prefix(COMMAND_HISTORY_CURSOR_PREFIX)
                .is_none_or(str::is_empty)
        {
            return Err(StorageError::new(
                StorageErrorCode::InvalidRequest,
                "invalid command history cursor",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the opaque cursor to pass to the next bounded read.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One station's newest-first retained command history, including any allowed child resources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandHistoryQuery {
    /// Canonical station identity (not a child or selected output target).
    pub station: ResourceRef,
    /// Continue from a prior page within the same station and grant scope.
    pub after: Option<CommandHistoryCursor>,
    /// Maximum entries on this page.
    pub limit: PageLimit,
}

/// Trusted resource predicate for a station history read. Storage applies it before LIMIT.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandHistoryScope {
    /// Station-wide grant covers this station and every child resource.
    pub descendants: bool,
    /// Exact station-only grant covers no child resources.
    pub station_only: bool,
    /// Exact child-resource grants (native protocol references do not widen grants).
    pub resources: Vec<CanonicalResource>,
}

impl CommandHistoryScope {
    /// Whether a returned canonical resource remains within the trusted scope.
    #[must_use]
    pub fn permits(&self, resource: &ResourceRef, station: &ResourceRef) -> bool {
        resource.bridge_id == station.bridge_id
            && resource.station_id == station.station_id
            && (self.descendants
                || match &resource.resource {
                    None => self.station_only,
                    Some(child) => self.resources.contains(child),
                })
    }

    /// Whether the scope grants any part of this station.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.descendants && !self.station_only && self.resources.is_empty()
    }
}

impl fmt::Display for CommandHistoryCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Error for CommandHistoryCursor {}
