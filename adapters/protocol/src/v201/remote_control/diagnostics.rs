//! Protected OCPP 2.0.1 `GetLog` (N01). The public request names no location; the provider
//! opens a bounded destination that is durably bound to the job before the native request can
//! reach the station.
#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
use super::{RemoteControlSession, ReservationGrant201, mapping};
use crate::SessionCallOutcome;
use crate::command_registry::diagnostics201;
use serde_json::{Map, Value, json};
use std::{sync::Arc, time::Duration};
use uob_application::{
    CommandDispatchOutcome, DiagnosticsJobMutation201, DiagnosticsStore201, RuntimeSecurityPolicy,
    artifact_provider::{ArtifactKind, ArtifactProvider, UploadRequest},
};
use uob_contracts::{
    Command, CommandError, CommandErrorCode, CommandOperation, DiagnosticsCallError201,
    DiagnosticsDestination201, DiagnosticsJob201, DiagnosticsJobState201, DiagnosticsReply201,
    DiagnosticsResult201, GetLogReference201, GetLogStatus201, LogType201, ProtocolEdition,
    StationSnapshot, UtcTimestamp, valid_log_file_name, valid_log_reason_code_201,
};

/// Longest provider or storage call before the request is refused without transmission.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(10);

/// Station-scoped log upload policy installed only for an explicitly enabled station.
pub struct DiagnosticsSettings201 {
    /// Bridge deadline after admission for the station to finish the upload.
    pub job_timeout: Duration,
    /// Largest accepted upload; the provider may lower it.
    pub maximum_upload_bytes: u64,
    pub artifacts: Arc<dyn ArtifactProvider>,
    /// Durable job owner; binds the destination before dispatch.
    pub jobs: Arc<dyn DiagnosticsStore201>,
    pub policy: RuntimeSecurityPolicy,
    pub grant: Arc<ReservationGrant201>,
}

/// Whether this module owns the command's dispatch.
pub(super) fn owns(command: &Command<Value>) -> bool {
    matches!(&command.operation, CommandOperation::Ocpp(operation)
        if operation.protocol == ProtocolEdition::Ocpp201
            && operation.action.as_str() == diagnostics201::ACTION)
}

struct Prepared {
    payload: Value,
    destination: DiagnosticsDestination201,
}

/// A request refused before any byte reached the station.
#[derive(Clone, Copy)]
struct Refusal(CommandErrorCode, &'static str);

impl From<Refusal> for CommandDispatchOutcome {
    fn from(Refusal(code, detail): Refusal) -> Self {
        Self::NotTransmitted {
            error: CommandError {
                code,
                detail: Some(detail.to_owned()),
            },
        }
    }
}

const fn unavailable() -> Refusal {
    Refusal(
        CommandErrorCode::PolicyRejected,
        "log upload destination unavailable",
    )
}

impl RemoteControlSession {
    /// Enables protected log retrieval for this authenticated socket only.
    #[must_use]
    pub fn with_diagnostics_201(mut self, settings: Arc<DiagnosticsSettings201>) -> Self {
        self.diagnostics = Some(settings);
        self
    }

    fn diagnostics_request(
        &self,
        command: &Command<Value>,
        snapshot: &StationSnapshot,
        now: UtcTimestamp,
    ) -> Result<Option<(&DiagnosticsSettings201, GetLogReference201)>, CommandErrorCode> {
        let CommandOperation::Ocpp(operation) = &command.operation else {
            return Ok(None);
        };
        if !owns(command) {
            return Ok(None);
        }
        let settings = self
            .diagnostics
            .as_deref()
            .ok_or(CommandErrorCode::UnsupportedOperation)?;
        let request = diagnostics201::validate(&command.resource, operation)?;
        if !(settings.grant)(command, now) {
            return Err(CommandErrorCode::Unauthorized);
        }
        uob_application::registration::v201::accepted(snapshot)
            .map_err(|_| CommandErrorCode::PolicyRejected)?;
        let capabilities = mapping::capabilities(snapshot, &command.resource)
            .ok_or(CommandErrorCode::InvalidParameters)?;
        command
            .validate_for_dispatch(capabilities, now)
            .map_err(|error| match error {
                uob_contracts::CommandValidationError::Expired => CommandErrorCode::Expired,
                uob_contracts::CommandValidationError::UnsupportedOperation(_) => {
                    CommandErrorCode::UnsupportedOperation
                }
            })?;
        if self.handle.is_closed() {
            return Err(CommandErrorCode::StationDisconnected);
        }
        Ok(Some((settings, request)))
    }

    /// Captures the durable job before admission; dispatch never starts without it.
    pub(super) fn diagnostics_context(
        &self,
        command: &Command<Value>,
        generation: Option<u64>,
        now: UtcTimestamp,
    ) -> Result<Option<DiagnosticsJobMutation201>, CommandErrorCode> {
        let snapshot = self
            .snapshot
            .read()
            .map_err(|_| CommandErrorCode::PolicyRejected)?;
        let Some((settings, request)) = self.diagnostics_request(command, &snapshot, now)? else {
            return Ok(None);
        };
        let deadline = settings
            .job_timeout
            .try_into()
            .ok()
            .and_then(|timeout| now.into_inner().checked_add(timeout))
            .map(UtcTimestamp::new)
            .ok_or(CommandErrorCode::InvalidParameters)?;
        Ok(Some(DiagnosticsJobMutation201 {
            station: snapshot.station.clone(),
            request_id: command.request_id.clone(),
            native_request_id: request.request_id,
            log_type: request.log_type,
            admitted_at: now,
            deadline,
            generation: generation.ok_or(CommandErrorCode::StationDisconnected)?,
        }))
    }

    /// Opens and binds the destination, sends exactly one native request and maps its reply.
    pub(super) async fn dispatch_diagnostics(
        &self,
        command: &Command<Value>,
    ) -> CommandDispatchOutcome {
        let now = self.clock.now();
        let checked = match self.snapshot.read() {
            Ok(snapshot) => self.diagnostics_request(command, &snapshot, now),
            Err(_) => Err(CommandErrorCode::PolicyRejected),
        };
        let (settings, request) = match checked {
            Ok(Some(found)) => found,
            Ok(None) => return mapping::not_sent(CommandErrorCode::InvalidParameters),
            Err(code) => return mapping::not_sent(code),
        };
        let prepared = match prepare(settings, command, &request).await {
            Ok(prepared) => prepared,
            Err(refusal) => return refusal.into(),
        };
        // Recheck authority after the provider calls, under the snapshot lock.
        let pending = match self.enqueue_rechecked(
            command,
            diagnostics201::ACTION,
            prepared.payload,
            |snapshot, now| self.diagnostics_request(command, snapshot, now).map(|_| ()),
        ) {
            Ok(pending) => pending,
            Err(code) => return mapping::not_sent(code),
        };
        let reply = match pending.receive().await {
            SessionCallOutcome::Result { payload, .. } => match native_reply(&payload) {
                Some(reply) => reply,
                None => return mapping::uncertain(),
            },
            SessionCallOutcome::Error { error, .. } => DiagnosticsReply201::CallError {
                code: DiagnosticsCallError201::from_code(&error.code),
            },
            SessionCallOutcome::NotTransmitted { reason, .. } => {
                return mapping::not_sent(if reason == "command expired before socket send" {
                    CommandErrorCode::Expired
                } else {
                    CommandErrorCode::PolicyRejected
                });
            }
            SessionCallOutcome::TimedOut { .. }
            | SessionCallOutcome::TransmissionUncertain { .. } => return mapping::uncertain(),
        };
        // Storage replaces this placeholder with the durable job revision.
        CommandDispatchOutcome::DiagnosticsResponse201(DiagnosticsResult201 {
            log_type: request.log_type,
            request_id: request.request_id,
            destination: Some(prepared.destination),
            reply: Some(reply),
            job: DiagnosticsJob201 {
                revision: 0,
                state: DiagnosticsJobState201::Pending,
                deadline: now,
                observed_at: now,
                last_status: None,
                last_status_at: None,
                notifications: 0,
                upload: None,
            },
        })
    }
}

/// Exact `GetLogResponse`; anything else leaves delivery uncertain.
pub(crate) fn native_reply(payload: &Value) -> Option<DiagnosticsReply201> {
    fn custom_data(value: Option<&Value>) -> bool {
        value.is_none_or(|data| {
            data.get("vendorId")
                .and_then(Value::as_str)
                .is_some_and(|vendor| vendor.chars().count() <= 255)
        })
    }
    fn only(object: &Map<String, Value>, allowed: &[&str]) -> bool {
        object.keys().all(|key| allowed.contains(&key.as_str()))
            && custom_data(object.get("customData"))
    }
    let object = payload.as_object()?;
    if !only(object, &["status", "statusInfo", "filename", "customData"]) {
        return None;
    }
    let status: GetLogStatus201 = serde_json::from_value(object.get("status")?.clone()).ok()?;
    let file_name = match object.get("filename") {
        None => None,
        Some(Value::String(name)) if valid_log_file_name(name) => Some(name.clone()),
        Some(_) => return None,
    };
    let reason_code = match object.get("statusInfo") {
        None => None,
        Some(info) => {
            let info = info.as_object()?;
            let additional = info.get("additionalInfo");
            if !only(info, &["reasonCode", "additionalInfo", "customData"])
                || additional.is_some_and(|text| text.as_str().is_none_or(|t| t.len() > 512))
            {
                return None;
            }
            let code = info.get("reasonCode")?.as_str()?;
            if !valid_log_reason_code_201(code) {
                return None;
            }
            Some(code.to_owned())
        }
    };
    Some(DiagnosticsReply201::Status {
        status,
        file_name,
        reason_code,
    })
}

async fn prepare(
    settings: &DiagnosticsSettings201,
    command: &Command<Value>,
    request: &GetLogReference201,
) -> Result<Prepared, Refusal> {
    let kind = match request.log_type {
        LogType201::DiagnosticsLog => ArtifactKind::DiagnosticsLog,
        LogType201::SecurityLog => ArtifactKind::SecurityLog,
    };
    let refused_test = || {
        Refusal(
            CommandErrorCode::PolicyRejected,
            "test log destination refused in this environment",
        )
    };
    if settings
        .policy
        .authorize_artifact_provider(settings.artifacts.descriptor())
        .is_err()
    {
        return Err(refused_test());
    }
    let destination = tokio::time::timeout(
        PROVIDER_TIMEOUT,
        settings.artifacts.open_upload(UploadRequest {
            kind,
            maximum_bytes: settings.maximum_upload_bytes,
        }),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| unavailable())?;
    if settings
        .policy
        .authorize_provider_material(destination.test_only)
        .is_err()
    {
        return Err(refused_test());
    }
    if destination.kind != kind
        || destination.maximum_bytes == 0
        || destination.maximum_bytes > settings.maximum_upload_bytes
    {
        return Err(unavailable());
    }
    // The binding must be durable before any byte reaches the station, so every reported
    // upload can later be checked against what the provider stored.
    tokio::time::timeout(
        PROVIDER_TIMEOUT,
        settings.jobs.bind_diagnostics_upload_201(
            command.request_id.clone(),
            destination.upload_id.as_str().to_owned(),
        ),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| unavailable())?;
    let mut log = json!({ "remoteLocation": destination.location.as_str() });
    if let Some(oldest) = request.oldest_timestamp {
        log["oldestTimestamp"] = json!(oldest);
    }
    if let Some(latest) = request.latest_timestamp {
        log["latestTimestamp"] = json!(latest);
    }
    let mut payload = json!({
        "log": log,
        "logType": request.log_type,
        "requestId": request.request_id,
    });
    if let Some(retries) = request.retries {
        payload["retries"] = json!(retries);
    }
    if let Some(interval) = request.retry_interval {
        payload["retryInterval"] = json!(interval);
    }
    serde_json::from_value::<rust_ocpp::v2_0_1::messages::get_log::GetLogRequest>(payload.clone())
        .map_err(|_| unavailable())?;
    Ok(Prepared {
        payload,
        destination: DiagnosticsDestination201 {
            log_type: request.log_type,
            maximum_bytes: destination.maximum_bytes,
            test_only: destination.test_only,
        },
    })
}
