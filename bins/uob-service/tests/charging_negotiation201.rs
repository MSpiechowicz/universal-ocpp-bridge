#![cfg(unix)]
#[allow(dead_code)]
#[path = "composite_schedule/support.rs"]
mod host;
#[path = "charging_negotiation201/limits.rs"]
mod limits;
#[path = "charging_negotiation201/needs.rs"]
mod needs;
#[path = "charging_negotiation201/support.rs"]
mod support;
