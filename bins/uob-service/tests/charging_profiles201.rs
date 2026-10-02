#![cfg(unix)]
#[allow(dead_code)] // Existing private real-daemon harness covers other action families too.
#[path = "composite_schedule/support.rs"]
mod daemon;
#[path = "charging_profiles201/lifecycle.rs"]
mod lifecycle;
#[allow(dead_code)] // Independent peer also powers the parent process smoke harness.
#[path = "charging_profiles201/peer.rs"]
mod peer;
#[path = "charging_profiles201/recovery.rs"]
mod recovery;
#[path = "charging_profiles201/support.rs"]
mod support;
