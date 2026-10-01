#![cfg(unix)]

#[path = "composite_schedule/admission.rs"]
mod admission;
#[path = "composite_schedule/native.rs"]
mod native;
#[path = "composite_schedule/recovery.rs"]
mod recovery;
#[path = "composite_schedule/support.rs"]
mod support;
