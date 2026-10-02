#![cfg(unix)]
#[path = "charging_profiles/admission.rs"]
mod admission;
#[allow(dead_code)] // Reuse the existing real-daemon lifecycle and private-layout harness.
#[path = "composite_schedule/support.rs"]
mod daemon;
#[path = "charging_profiles/native.rs"]
mod native;
#[path = "charging_profiles/peer.rs"]
mod peer;
#[path = "charging_profiles/recovery.rs"]
mod recovery;
#[path = "charging_profiles/support.rs"]
mod support;
