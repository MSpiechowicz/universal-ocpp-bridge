#![doc = "External provider boundary for authorization, PKI, artifacts, and payments."]

/// Marker for providers selected by the composition root.
pub trait ProviderAdapter: Send + Sync {
    /// Stable provider kind.
    fn kind(&self) -> &'static str;
}
