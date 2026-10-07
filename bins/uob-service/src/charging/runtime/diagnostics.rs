//! Native OCPP 1.6 diagnostics and Security Whitepaper log status notifications. A station's
//! `Uploaded` claim is checked against the provider before the job fact, together with any
//! pending trigger evidence, commits in one write ahead of the empty CALLRESULT.
use std::{fmt::Write, io, time::Duration};

use uob_application::{
    AtomicStoreWrite, ChargerObservation, CommandClock, DiagnosticsObservation16,
    DiagnosticsObservationKind16, DiagnosticsStore16, OperationalStore, UploadCheck16,
    UploadOutcome16,
    artifact_provider::{ArtifactProvider, UploadId, UploadStatus},
    attribute_diagnostics_16,
};
use uob_contracts::{
    DiagnosticsUpload16, LogUploadStatus16, ProtocolEdition, StationSnapshot, TriggerMessageClass,
};
use uob_protocol_adapter::{IncomingCall, OcppCallError, OcppErrorCode};

use super::{CallContext, Clock, CommitState, call_error, trigger, unavailable};

/// Longest provider check before the claim is recorded as unconfirmed.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) async fn complete_status(
    incoming: &IncomingCall,
    snapshot: &StationSnapshot,
    services: &CallContext<'_>,
    commits: &mut CommitState,
) -> io::Result<Result<serde_json::Value, OcppCallError>> {
    let protocol = ProtocolEdition::Ocpp16j;
    if uob_application::registration::accepted(snapshot).is_err() {
        return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
    }
    let (status, log, request_id, trigger_status) = match &incoming.call.observation {
        ChargerObservation::TriggerStatus {
            class: TriggerMessageClass::DiagnosticsStatusNotification,
            status,
        } => {
            let Ok(native) = serde_json::from_value::<LogUploadStatus16>(status.as_str().into())
            else {
                return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
            };
            if !native.diagnostics() {
                return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
            }
            (native, false, None, Some(status.as_str()))
        }
        ChargerObservation::LogStatus16 { status, request_id } => {
            (*status, true, *request_id, None)
        }
        _ => return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError))),
    };
    // The Security Whitepaper message exists only where its workflow was explicitly enabled.
    let Some((_, providers)) = services.diagnostics.filter(|(settings, _)| {
        if log {
            settings.log
        } else {
            settings.diagnostics
        }
    }) else {
        return Ok(Err(call_error(protocol, OcppErrorCode::NotImplemented)));
    };
    let now = Clock.now();
    let upload = if status == LogUploadStatus16::Uploaded {
        check_upload(
            services,
            providers.artifacts(),
            snapshot,
            status,
            log,
            request_id,
        )
        .await?
    } else {
        None
    };
    let mut write = AtomicStoreWrite::empty();
    write
        .diagnostics_observations_16
        .push(DiagnosticsObservation16 {
            station: snapshot.station.clone(),
            observed_at: now,
            kind: DiagnosticsObservationKind16::Status {
                status,
                log,
                request_id,
                upload,
            },
        });
    if let Some(status) = trigger_status.filter(|_| services.trigger_enabled)
        && let Some(marker) = trigger::marker(
            services.store,
            &snapshot.station,
            services.identity,
            TriggerMessageClass::DiagnosticsStatusNotification,
            None,
            Some(status),
            now,
        )
        .await?
    {
        write.journal_events.push(marker);
        commits.trigger_committed = true;
    }
    services
        .store
        .write_atomic(write)
        .await
        .map_err(|_| unavailable())?;
    Ok(Ok(serde_json::json!([3, incoming.call.message_id, {}])))
}

/// What the provider stored for the destination of the job this notification belongs to.
async fn check_upload(
    services: &CallContext<'_>,
    artifacts: &impl ArtifactProvider,
    snapshot: &StationSnapshot,
    status: LogUploadStatus16,
    log: bool,
    request_id: Option<i32>,
) -> io::Result<Option<UploadCheck16>> {
    let records = services
        .store
        .diagnostics_jobs_16(snapshot.station.clone())
        .await
        .map_err(|_| unavailable())?;
    let Some(upload_id) = attribute_diagnostics_16(&records, status, log, request_id)
        .and_then(|index| records[index].upload_id.clone())
    else {
        return Ok(None);
    };
    let outcome = match stored_upload(artifacts, upload_id.as_str()).await {
        Some((sha256, size_bytes)) => {
            UploadOutcome16::Received(DiagnosticsUpload16 { sha256, size_bytes })
        }
        None => UploadOutcome16::Missing,
    };
    Ok(Some(UploadCheck16 { upload_id, outcome }))
}

/// Lowercase SHA-256 and size of the complete file the provider holds for one destination, or
/// None for an incomplete or unknown upload and for an unavailable provider.
pub(super) async fn stored_upload(
    artifacts: &impl ArtifactProvider,
    upload_id: &str,
) -> Option<(String, u64)> {
    let identity = UploadId::new(upload_id).ok()?;
    match tokio::time::timeout(PROVIDER_TIMEOUT, artifacts.upload_status(&identity)).await {
        Ok(Ok(UploadStatus::Received { size_bytes, sha256 })) => Some((
            sha256
                .as_bytes()
                .iter()
                .fold(String::with_capacity(64), |mut hex, byte| {
                    let _ = write!(hex, "{byte:02x}");
                    hex
                }),
            size_bytes,
        )),
        _ => None,
    }
}
