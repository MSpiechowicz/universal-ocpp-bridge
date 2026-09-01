#![doc = "Operational storage adapter boundary. Driver types remain in this package."]

/// Marker for operational storage implementations selected at composition time.
pub trait OperationalStorageAdapter: Send + Sync {
    /// Stable implementation kind.
    fn kind(&self) -> &'static str;
}
