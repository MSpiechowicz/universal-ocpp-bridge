#![doc = "Protocol adapter boundary. Concrete protocol model crates belong here."]

mod station;

pub use station::{
    OrderedStationOutput, SpawnedStation, StationHandle, StationOutputKind, StationOutputReceiver,
    StationOutputs, StationSendError, StationTask, StationTaskError, spawn_station,
};

/// Marker implemented by protocol adapters registered by the service.
pub trait ProtocolAdapter: Send + Sync {
    /// Stable adapter kind used by the composition root.
    fn kind(&self) -> &'static str;
}
