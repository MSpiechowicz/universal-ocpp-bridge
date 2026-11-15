//! Privileged OCPP 2.0.1 monitoring (N02 to N08) over a real authenticated socket. The durable
//! report store is a recording fake: the protocol adapter's contract is what it asks the store
//! to finish, not how storage merges.
#[path = "ocpp201_monitoring/commands.rs"]
mod commands;
#[path = "ocpp201_monitoring/commands_limits.rs"]
mod commands_limits;
mod endpoint_support;
#[path = "ocpp201_monitoring/events.rs"]
mod events;
#[path = "ocpp201_monitoring/harness.rs"]
mod harness;
#[path = "ocpp201_monitoring/reports.rs"]
mod reports;
#[path = "ocpp201_monitoring/reports_failures.rs"]
mod reports_failures;
#[path = "ocpp201_monitoring/reports_limits.rs"]
mod reports_limits;
#[allow(dead_code)] // Independently compiled roots share this opt-in socket harness.
#[path = "ocpp201_remote_control/support.rs"]
mod support;
