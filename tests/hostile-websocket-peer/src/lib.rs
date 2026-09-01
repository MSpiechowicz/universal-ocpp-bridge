//! Low-level WebSocket peer for negative OCPP transport tests.
//!
//! This crate intentionally has no OCPP model or bridge implementation dependency. Payloads are
//! opaque bytes so invalid JSON and invalid OCPP shapes can reach the system under test unchanged.

mod model;
mod peer;
mod runner;

pub use model::{Action, ExpectedFrame, Payload, Scenario, ScenarioError, Step, parse_scenario};
pub use peer::{Direction, Observation, ObservationKind, Peer, PeerConfig, PeerError};
pub use runner::{RunFailure, RunReport, ScenarioRunner, StepResult};

pub const SCHEMA_VERSION: u16 = 1;
