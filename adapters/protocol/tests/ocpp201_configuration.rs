#[path = "ocpp201_configuration/support.rs"]
mod configuration_support;
#[path = "ocpp201_configuration/diagnostics.rs"]
mod diagnostics;
mod endpoint_support;
#[path = "ocpp201_configuration/lifecycle.rs"]
mod lifecycle;
#[path = "ocpp201_configuration/network.rs"]
mod network;
#[path = "ocpp201_configuration/recovery.rs"]
mod recovery;
#[allow(dead_code)]
#[path = "ocpp201_remote_control/support.rs"]
mod support;
#[path = "ocpp201_configuration/variables.rs"]
mod variables;
