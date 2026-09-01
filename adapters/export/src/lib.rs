#![doc = "External export adapter boundary. Provider drivers remain in this package."]

/// Marker for passive external export implementations.
pub trait ExternalExportAdapter: Send + Sync {
    /// Stable provider kind.
    fn kind(&self) -> &'static str;
}
