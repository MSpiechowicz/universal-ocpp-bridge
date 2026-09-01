mod cli;
mod fault;
mod model;
mod report;
mod runner;
mod scheduling;
mod state;

pub use cli::{CliResult, execute};
pub use model::{
    ActionKind, ConfiguredOcppVersion, EvseDefinition, FaultDefinition, FaultKind,
    ScenarioDefinition, SimulatorConfiguration, StationDefinition, StepDefinition,
    parse_configuration, parse_scenario,
};
pub use report::{DiagnosticCounts, FailureCategory, RunEvent, RunFailure, RunReport};
pub use runner::{
    CancellationHandle, CancellationToken, ConnectFuture, RealClock, RealConnector, ScenarioClock,
    ScenarioConnector, ScenarioRunner, SleepFuture, cancellation_pair,
};
pub use state::{ResourceState, StationResource, StationState, StationStateError};

pub const SCHEMA_VERSION: u16 = 1;
