//! Demo-only OCPP 1.6 diagnostics, Security Whitepaper log and OCPP 2.0.1 log upload options.
//! Uploads go to the same local test artifact service as firmware (`[charging.firmware]`).
use std::time::Duration;

use uob_contracts::ProtocolEdition;

use super::{ConfigurationLoadError, StationControlOptions};

const MIN_JOB_TIMEOUT_SECONDS: u64 = 60;
const MAX_JOB_TIMEOUT_SECONDS: u64 = 24 * 60 * 60;
/// Default and largest accepted upload; the artifact transfers cap every body at 32 MiB.
const DEFAULT_UPLOAD_BYTES: u64 = 8 * 1024 * 1024;
const MAX_UPLOAD_BYTES: u64 = 32 * 1024 * 1024;

/// Enabled native log families of one station and its upload bounds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StationDiagnostics {
    /// OCPP 1.6 `GetDiagnostics`.
    pub diagnostics: bool,
    /// Security Whitepaper `GetLog` on OCPP 1.6, or OCPP 2.0.1 `GetLog`.
    pub log: bool,
    pub job_timeout: Duration,
    pub maximum_upload_bytes: u64,
}

pub(super) fn station(
    control: &StationControlOptions,
    timeout_seconds: Option<u64>,
    maximum_upload_bytes: Option<u64>,
    protocol: ProtocolEdition,
) -> Result<Option<StationDiagnostics>, ConfigurationLoadError> {
    let fail = ConfigurationLoadError::InvalidCharging;
    let (diagnostics, log) = (control.get_diagnostics.enabled(), control.get_log.enabled());
    if !diagnostics && !log {
        return if timeout_seconds.is_some() || maximum_upload_bytes.is_some() {
            Err(fail)
        } else {
            Ok(None)
        };
    }
    // `GetDiagnostics` exists only in OCPP 1.6; OCPP 2.0.1 retrieves every log with `GetLog`.
    if protocol != ProtocolEdition::Ocpp16j && diagnostics {
        return Err(fail);
    }
    let timeout = timeout_seconds.ok_or(fail)?;
    let maximum_upload_bytes = maximum_upload_bytes.unwrap_or(DEFAULT_UPLOAD_BYTES);
    if !(MIN_JOB_TIMEOUT_SECONDS..=MAX_JOB_TIMEOUT_SECONDS).contains(&timeout)
        || !(1..=MAX_UPLOAD_BYTES).contains(&maximum_upload_bytes)
    {
        return Err(fail);
    }
    Ok(Some(StationDiagnostics {
        diagnostics,
        log,
        job_timeout: Duration::from_secs(timeout),
        maximum_upload_bytes,
    }))
}
