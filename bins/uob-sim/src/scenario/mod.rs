mod cli;
mod model;
mod report;
mod runner;

pub use cli::{CliResult, execute};
pub use model::{
    ActionKind, ScenarioDefinition, SimulatorConfiguration, StationDefinition, StepDefinition,
    parse_configuration, parse_scenario,
};
pub use report::{FailureCategory, RunEvent, RunFailure, RunReport};
pub use runner::{
    CancellationHandle, CancellationToken, ConnectFuture, RealClock, RealConnector, ScenarioClock,
    ScenarioConnector, ScenarioRunner, SleepFuture, cancellation_pair,
};

pub const SCHEMA_VERSION: u16 = 1;
