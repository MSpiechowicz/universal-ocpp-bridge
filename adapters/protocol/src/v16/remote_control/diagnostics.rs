//! Protected OCPP 1.6 `GetDiagnostics` and Security Whitepaper `GetLog`. The public request
//! names no location; the provider opens a bounded destination that is durably bound to the
//! job before the native request can reach the station.
use super::{RemoteControlSession, ReservationGrant16, mapping};
use crate::SessionCallOutcome;
use crate::command_registry::diagnostics16::{self, Request};
use serde_json::{Map, Value, json};
use std::{sync::Arc, time::Duration};
use uob_application::{
    CommandDispatchOutcome, DiagnosticsJobMutation16, DiagnosticsStore16, DiagnosticsVariant16,
    RuntimeSecurityPolicy, StationCommandError,
    artifact_provider::{ArtifactKind, ArtifactProvider, UploadRequest},
};
use uob_contracts::{
    Command, CommandError, CommandErrorCode, CommandOperation, DiagnosticsCallError16,
    DiagnosticsDestination16, DiagnosticsJob16, DiagnosticsJobState16, DiagnosticsReply16,
    DiagnosticsResult16, GetLogStatus16, LogType16, StationSnapshot, UtcTimestamp,
    valid_log_file_name,
};

/// Longest provider or storage call before the request is refused without transmission.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(10);

/// Station-scoped log upload policy installed only for an explicitly enabled station.
pub struct DiagnosticsSettings16 {
    /// OCPP 1.6 `GetDiagnostics` is enabled.
    pub diagnostics: bool,
    /// Security Whitepaper `GetLog` is enabled.
    pub log: bool,
    /// Bridge deadline after admission for the station to finish the upload.
    pub job_timeout: Duration,
    /// Largest accepted upload; the provider may lower it.
    pub maximum_upload_bytes: u64,
    pub artifacts: Arc<dyn ArtifactProvider>,
    /// Durable job owner; binds the destination before dispatch.
    pub jobs: Arc<dyn DiagnosticsStore16>,
    pub policy: RuntimeSecurityPolicy,
    pub grant: Arc<ReservationGrant16>,
}

struct Prepared {
    payload: Value,
    destination: DiagnosticsDestination16,
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
    fn diagnostics_request(
        &self,
        command: &Command<Value>,
        snapshot: &StationSnapshot,
        now: UtcTimestamp,
    ) -> Result<Option<(&DiagnosticsSettings16, Request)>, CommandErrorCode> {
        let CommandOperation::Ocpp(operation) = &command.operation else {
            return Ok(None);
        };
        if !diagnostics16::owns(operation) {
            return Ok(None);
        }
        let settings = self
            .diagnostics
            .as_deref()
            .ok_or(CommandErrorCode::UnsupportedOperation)?;
        let request = diagnostics16::validate(&command.resource, operation)?;
        if !(settings.grant)(command, now) {
            return Err(CommandErrorCode::Unauthorized);
        }
        let enabled = match request {
            Request::Diagnostics(_) => settings.diagnostics,
            Request::Log(_) => settings.log,
        };
        if !enabled {
            return Err(CommandErrorCode::UnsupportedOperation);
        }
        uob_application::registration::accepted(snapshot)
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
    ) -> Result<Option<DiagnosticsJobMutation16>, CommandErrorCode> {
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
        Ok(Some(DiagnosticsJobMutation16 {
            station: snapshot.station.clone(),
            request_id: command.request_id.clone(),
            variant: match &request {
                Request::Diagnostics(_) => DiagnosticsVariant16::Diagnostics,
                Request::Log(request) => DiagnosticsVariant16::Log {
                    log_type: request.log_type,
                    request_id: request.request_id,
                },
            },
            admitted_at: now,
            deadline,
            generation: generation.ok_or(CommandErrorCode::StationDisconnected)?,
        }))
    }

    /// Opens and binds the destination, sends exactly one native request and maps its reply.
    pub(super) async fn dispatch_diagnostics(
        &self,
        command: &Command<Value>,
    ) -> Result<CommandDispatchOutcome, StationCommandError> {
        let now = self.clock.now();
        let (settings, request) = {
            let snapshot = self.snapshot.read().map_err(|_| super::state_error())?;
            match self.diagnostics_request(command, &snapshot, now) {
                Ok(Some(found)) => found,
                Ok(None) => return Ok(mapping::not_sent(CommandErrorCode::InvalidParameters)),
                Err(code) => return Ok(mapping::not_sent(code)),
            }
        };
        let prepared = match prepare(settings, command, &request).await {
            Ok(prepared) => prepared,
            Err(refusal) => return Ok(refusal.into()),
        };
        let pending = match self.enqueue_prepared(
            command,
            request.action(),
            prepared.payload,
            self.clock.now(),
        ) {
            Ok(pending) => pending,
            Err(code) => return Ok(mapping::not_sent(code)),
        };
        let reply = match pending.receive().await {
            SessionCallOutcome::Result { payload, .. } => match native_reply(&request, &payload) {
                Some(reply) => reply,
                None => return Ok(mapping::uncertain()),
            },
            SessionCallOutcome::Error { error, .. } => DiagnosticsReply16::CallError {
                code: DiagnosticsCallError16::from_code(&error.code),
            },
            SessionCallOutcome::NotTransmitted { reason, .. } => {
                return Ok(mapping::not_sent(
                    if reason == "command expired before socket send" {
                        CommandErrorCode::Expired
                    } else {
                        CommandErrorCode::PolicyRejected
                    },
                ));
            }
            SessionCallOutcome::TimedOut { .. }
            | SessionCallOutcome::TransmissionUncertain { .. } => return Ok(mapping::uncertain()),
        };
        Ok(CommandDispatchOutcome::DiagnosticsResponse16(result(
            &request,
            prepared.destination,
            reply,
            now,
        )))
    }
}

/// Maps the exact native reply; anything outside the schema is not a correlated answer.
pub(super) fn native_reply(request: &Request, payload: &Value) -> Option<DiagnosticsReply16> {
    let object = payload.as_object()?;
    let file_name = |object: &Map<String, Value>, key: &str| -> Option<Option<String>> {
        match object.get(key) {
            None => Some(None),
            Some(Value::String(name)) if valid_log_file_name(name) => Some(Some(name.clone())),
            Some(_) => None,
        }
    };
    match request {
        Request::Diagnostics(_) => {
            if object.keys().any(|key| key != "fileName") {
                return None;
            }
            Some(DiagnosticsReply16::Diagnostics {
                file_name: file_name(object, "fileName")?,
            })
        }
        Request::Log(_) => {
            if object
                .keys()
                .any(|key| key != "status" && key != "filename")
            {
                return None;
            }
            let status: GetLogStatus16 =
                serde_json::from_value(object.get("status")?.clone()).ok()?;
            Some(DiagnosticsReply16::Log {
                status,
                file_name: file_name(object, "filename")?,
            })
        }
    }
}

fn result(
    request: &Request,
    destination: DiagnosticsDestination16,
    reply: DiagnosticsReply16,
    now: UtcTimestamp,
) -> DiagnosticsResult16 {
    // Storage replaces this placeholder with the durable job revision.
    let job = DiagnosticsJob16 {
        revision: 0,
        state: DiagnosticsJobState16::Pending,
        deadline: now,
        observed_at: now,
        last_status: None,
        last_status_at: None,
        notifications: 0,
        rejected_transitions: 0,
        upload: None,
    };
    match request {
        Request::Diagnostics(_) => DiagnosticsResult16::GetDiagnostics {
            destination: Some(destination),
            reply: Some(reply),
            job,
        },
        Request::Log(log) => DiagnosticsResult16::GetLog {
            log_type: log.log_type,
            request_id: log.request_id,
            destination: Some(destination),
            reply: Some(reply),
            job,
        },
    }
}

async fn prepare(
    settings: &DiagnosticsSettings16,
    command: &Command<Value>,
    request: &Request,
) -> Result<Prepared, Refusal> {
    let kind = match request.log_type() {
        LogType16::DiagnosticsLog => ArtifactKind::DiagnosticsLog,
        LogType16::SecurityLog => ArtifactKind::SecurityLog,
    };
    if settings
        .policy
        .authorize_artifact_provider(settings.artifacts.descriptor())
        .is_err()
    {
        return Err(Refusal(
            CommandErrorCode::PolicyRejected,
            "test log destination refused in this environment",
        ));
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
        return Err(Refusal(
            CommandErrorCode::PolicyRejected,
            "test log destination refused in this environment",
        ));
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
        settings.jobs.bind_diagnostics_upload_16(
            command.request_id.clone(),
            destination.upload_id.as_str().to_owned(),
        ),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| unavailable())?;
    let location = destination.location.as_str();
    let payload = match request {
        Request::Diagnostics(diagnostics) => {
            let mut payload = json!({ "location": location });
            if let Some(start) = diagnostics.start_time {
                payload["startTime"] = json!(start);
            }
            if let Some(stop) = diagnostics.stop_time {
                payload["stopTime"] = json!(stop);
            }
            optional_counts(
                &mut payload,
                diagnostics.retries,
                diagnostics.retry_interval,
            );
            let native: rust_ocpp::v1_6::messages::get_diagnostics::GetDiagnosticsRequest =
                serde_json::from_value(payload.clone()).map_err(|_| unavailable())?;
            validator::Validate::validate(&native).map_err(|_| unavailable())?;
            payload
        }
        Request::Log(log) => {
            let mut parameters = json!({ "remoteLocation": location });
            if let Some(oldest) = log.oldest_timestamp {
                parameters["oldestTimestamp"] = json!(oldest);
            }
            if let Some(latest) = log.latest_timestamp {
                parameters["latestTimestamp"] = json!(latest);
            }
            let mut payload = json!({
                "log": parameters,
                "logType": log.log_type,
                "requestId": log.request_id,
            });
            optional_counts(&mut payload, log.retries, log.retry_interval);
            payload
        }
    };
    Ok(Prepared {
        payload,
        destination: DiagnosticsDestination16 {
            log_type: request.log_type(),
            maximum_bytes: destination.maximum_bytes,
            test_only: destination.test_only,
        },
    })
}

fn optional_counts(payload: &mut Value, retries: Option<u32>, interval: Option<u32>) {
    if let Some(retries) = retries {
        payload["retries"] = json!(retries);
    }
    if let Some(interval) = interval {
        payload["retryInterval"] = json!(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::{Request, native_reply};
    use serde_json::json;
    use uob_contracts::{
        DiagnosticsReply16, GetDiagnosticsReference16, GetLogReference16, GetLogStatus16, LogType16,
    };

    fn log() -> Request {
        Request::Log(GetLogReference16 {
            log_type: LogType16::SecurityLog,
            request_id: 1,
            oldest_timestamp: None,
            latest_timestamp: None,
            retries: None,
            retry_interval: None,
        })
    }

    #[test]
    fn only_exact_native_replies_are_correlated_answers() {
        let diagnostics = Request::Diagnostics(GetDiagnosticsReference16::default());
        assert_eq!(
            native_reply(&diagnostics, &json!({})),
            Some(DiagnosticsReply16::Diagnostics { file_name: None })
        );
        assert_eq!(
            native_reply(&diagnostics, &json!({"fileName":"diag.zip"})),
            Some(DiagnosticsReply16::Diagnostics {
                file_name: Some("diag.zip".to_owned())
            })
        );
        assert_eq!(
            native_reply(
                &log(),
                &json!({"status":"AcceptedCanceled","filename":"s.log"})
            ),
            Some(DiagnosticsReply16::Log {
                status: GetLogStatus16::AcceptedCanceled,
                file_name: Some("s.log".to_owned())
            })
        );
        for (request, payload) in [
            (&diagnostics, json!({"fileName": "x".repeat(256)})),
            (&diagnostics, json!({"fileName": "tab\tname"})),
            (&diagnostics, json!({"fileName": null})),
            (&diagnostics, json!({"status": "Accepted"})),
            (&diagnostics, json!([])),
            (&log(), json!({})),
            (&log(), json!({"status":"Unknown"})),
            (&log(), json!({"status":"Accepted","fileName":"wrong-case"})),
            (&log(), json!({"status":"Accepted","filename":7})),
        ] {
            assert_eq!(native_reply(request, &payload), None, "{payload}");
        }
    }

    #[test]
    fn corpus_replies_map_and_negative_replies_are_never_answers() {
        let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/ocpp-fixtures/corpus/wire/1.6");
        let read = |name: &str| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(corpus.join(name)).unwrap()).unwrap()
        };
        let diagnostics = Request::Diagnostics(GetDiagnosticsReference16::default());
        for name in [
            "diagnostics-get-reply-file.json",
            "diagnostics-get-reply-no-file.json",
        ] {
            assert!(
                native_reply(&diagnostics, &read(name)[2]).is_some(),
                "{name}"
            );
        }
        for name in [
            "log-get-reply-accepted.json",
            "log-get-reply-accepted-no-filename.json",
            "log-get-reply-rejected.json",
            "log-get-reply-accepted-canceled.json",
        ] {
            assert!(native_reply(&log(), &read(name)[2]).is_some(), "{name}");
        }
        let cases = read("diagnostics-negative-cases.json");
        let mut refused = 0;
        for case in cases["cases"].as_array().unwrap() {
            if case["wire"][0] != 3 {
                continue;
            }
            let request = if case["schema"].as_str().unwrap().contains("GetLog") {
                log()
            } else {
                Request::Diagnostics(GetDiagnosticsReference16::default())
            };
            assert_eq!(
                native_reply(&request, &case["wire"][2]),
                None,
                "{}",
                case["id"]
            );
            refused += 1;
        }
        assert_eq!(refused, 6);
    }
}
