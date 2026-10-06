//! OCPP 2.0.1 composite schedules (K08) and installed-profile reports (K09) over an actual
//! authenticated socket, with durable SQLite evidence.
#![allow(dead_code)] // Existing live-socket fixture also serves other remote families.
#[path = "ocpp201_schedules/composite.rs"]
mod composite;
mod endpoint_support;
#[path = "ocpp201_schedules/fixtures.rs"]
mod fixtures;
#[path = "ocpp201_remote_control/support.rs"]
mod remote;
#[path = "ocpp201_schedules/reports.rs"]
mod reports;
#[path = "ocpp201_schedules/support.rs"]
mod schedules;
#[path = "ocpp201_schedules/validation.rs"]
mod validation;
use remote::*;
use schedules::*;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::time::timeout;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v201::remote_control::RemoteControlSession;
