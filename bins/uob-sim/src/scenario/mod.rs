mod action;
mod cli;
mod execution;
mod fault;
mod model;
mod report;
mod runner;
mod scheduling;
mod state;

pub use action::ActionKind;
pub use cli::{CliResult, execute};
pub use model::{
    ConfiguredOcppVersion, EvseDefinition, FaultDefinition, FaultKind, ScenarioDefinition,
    SimulatorConfiguration, StationDefinition, StepDefinition, parse_configuration, parse_scenario,
};
pub use report::{DiagnosticCounts, FailureCategory, RunEvent, RunFailure, RunReport};
pub use runner::{
    CancellationHandle, CancellationToken, ConnectFuture, RealClock, RealConnector, ScenarioClock,
    ScenarioConnector, ScenarioRunner, SleepFuture, cancellation_pair,
};
pub use state::{ResourceState, StationResource, StationState, StationStateError};

pub const SCHEMA_VERSION: u16 = 1;
