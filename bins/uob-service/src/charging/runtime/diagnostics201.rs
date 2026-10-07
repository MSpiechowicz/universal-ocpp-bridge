//! Native OCPP 2.0.1 `LogStatusNotification` (N01). A station's `Uploaded` claim is checked
//! against the provider before the job fact, together with any pending trigger evidence,
//! commits in one write ahead of the empty CALLRESULT.
use std::io;

use uob_application::{
    AtomicStoreWrite, ChargerObservation, CommandClock, DiagnosticsObservation201,
    DiagnosticsObservationKind201, DiagnosticsStore201, OperationalStore, UploadCheck201,
    UploadOutcome201, artifact_provider::ArtifactProvider, attribute_diagnostics_201,
};
use uob_contracts::{
    DiagnosticsUpload201, LogUploadStatus201, ProtocolEdition, StationSnapshot,
    TriggerMessageClass201, TriggerTarget201,
};
use uob_protocol_adapter::{IncomingCall, OcppCallError, OcppErrorCode};

use super::{
    CallContext, Clock, CommitState, call_error, diagnostics::stored_upload, trigger201,
    unavailable,
};

/// OCPP 2.0.1 `LogStatusNotification` for a log-enabled station. The report belongs only to the
/// job with its exact `requestId` (N01.FR.07); an unmatched one changes no job but is still
/// acknowledged and still counts as trigger evidence. The decoder already refused a non-`Idle`
/// report without `requestId`; the identity-free `Idle` is accepted only as the answer to a
/// pending `TriggerMessage` (N01.FR.13).
pub(super) async fn complete_status_201(
    incoming: &IncomingCall,
    snapshot: &StationSnapshot,
    services: &CallContext<'_>,
    commits: &mut CommitState,
) -> io::Result<Result<serde_json::Value, OcppCallError>> {
    let protocol = ProtocolEdition::Ocpp201;
    if uob_application::registration::v201::accepted(snapshot).is_err() {
        return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
    }
    let ChargerObservation::LogStatus201 { status, request_id } = incoming.call.observation else {
        return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
    };
    let Some((_, providers)) = services.diagnostics.filter(|(settings, _)| settings.log) else {
        return Ok(Err(call_error(protocol, OcppErrorCode::NotImplemented)));
    };
    let now = Clock.now();
    let marker = if services.trigger_enabled {
        trigger201::marker(
            services.store,
            &snapshot.station,
            services.identity,
            TriggerMessageClass201::LogStatusNotification,
            TriggerTarget201::Station,
            Some(status.as_str()),
            now,
        )
        .await?
    } else {
        None
    };
    if request_id.is_none() && marker.is_none() {
        return Ok(Err(call_error(
            protocol,
            OcppErrorCode::OccurrenceConstraintViolation,
        )));
    }
    let upload = if status == LogUploadStatus201::Uploaded {
        check_upload(
            services,
            providers.artifacts(),
            snapshot,
            status,
            request_id,
        )
        .await?
    } else {
        None
    };
    let mut write = AtomicStoreWrite::empty();
    write
        .diagnostics_observations_201
        .push(DiagnosticsObservation201 {
            station: snapshot.station.clone(),
            observed_at: now,
            kind: DiagnosticsObservationKind201::Status {
                status,
                request_id,
                upload,
            },
        });
    if let Some(marker) = marker {
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
    status: LogUploadStatus201,
    request_id: Option<i32>,
) -> io::Result<Option<UploadCheck201>> {
    let records = services
        .store
        .diagnostics_jobs_201(snapshot.station.clone())
        .await
        .map_err(|_| unavailable())?;
    let Some(upload_id) = attribute_diagnostics_201(&records, status, request_id)
        .and_then(|index| records[index].upload_id.clone())
    else {
        return Ok(None);
    };
    let outcome = match stored_upload(artifacts, upload_id.as_str()).await {
        Some((sha256, size_bytes)) => {
            UploadOutcome201::Received(DiagnosticsUpload201 { sha256, size_bytes })
        }
        None => UploadOutcome201::Missing,
    };
    Ok(Some(UploadCheck201 { upload_id, outcome }))
}
