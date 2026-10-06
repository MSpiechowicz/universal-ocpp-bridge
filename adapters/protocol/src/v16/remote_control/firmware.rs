//! Protected OCPP 1.6 `UpdateFirmware` and Security Whitepaper `SignedUpdateFirmware`.
//! The public request names an artifact; the provider supplies what the station needs.
use super::{RemoteControlSession, ReservationGrant16, mapping};
use crate::SessionCallOutcome;
use crate::command_registry::firmware16::{self, Request};
use serde_json::{Value, json};
use std::{fmt::Write, sync::Arc, time::Duration};
use uob_application::{
    CommandDispatchOutcome, FirmwareJobMutation16, FirmwareVariant16, RuntimeSecurityPolicy,
    StationCommandError,
    artifact_provider::{ArtifactKind, ArtifactProvider, ArtifactReference},
    certificate_provider::{
        CertificateChainPem, CertificateProvider, ChainPurpose, ChainVerification, TrustDecision,
    },
};
use uob_contracts::{
    Command, CommandError, CommandErrorCode, CommandOperation, FirmwareArtifact16,
    FirmwareCallError16, FirmwareJob16, FirmwareJobState16, FirmwareReply16, FirmwareResult16,
    SignedUpdateFirmwareStatus16, StationSnapshot, UtcTimestamp,
};

/// Longest provider or PKI call before the request is refused without transmission.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(10);

/// Station-scoped firmware policy installed only for an explicitly enabled station.
pub struct FirmwareSettings16 {
    /// Exactly one native family per station (L01.FR.20).
    pub signed: bool,
    /// Bridge deadline added to the latest native start instant.
    pub job_timeout: Duration,
    pub artifacts: Arc<dyn ArtifactProvider>,
    /// Required for signed firmware; verifies the signing certificate before sending (A00.FR.706).
    pub certificates: Option<Arc<dyn CertificateProvider>>,
    pub policy: RuntimeSecurityPolicy,
    pub grant: Arc<ReservationGrant16>,
}

struct Prepared {
    payload: Value,
    artifact: FirmwareArtifact16,
}

/// A request refused before any byte reached the station.
#[derive(Clone, Copy)]
struct Refusal(CommandErrorCode, &'static str);

const fn refused(code: CommandErrorCode, detail: &'static str) -> Refusal {
    Refusal(code, detail)
}

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

impl RemoteControlSession {
    fn firmware_request(
        &self,
        command: &Command<Value>,
        snapshot: &StationSnapshot,
        now: UtcTimestamp,
    ) -> Result<Option<(&FirmwareSettings16, Request)>, CommandErrorCode> {
        let CommandOperation::Ocpp(operation) = &command.operation else {
            return Ok(None);
        };
        if !firmware16::ACTIONS.contains(&operation.action.as_str()) {
            return Ok(None);
        }
        let settings = self
            .firmware
            .as_deref()
            .ok_or(CommandErrorCode::UnsupportedOperation)?;
        let request = firmware16::validate(&command.resource, operation)?;
        if !(settings.grant)(command, now) {
            return Err(CommandErrorCode::Unauthorized);
        }
        if matches!(request, Request::Signed(_)) != settings.signed {
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
    pub(super) fn firmware_context(
        &self,
        command: &Command<Value>,
        generation: Option<u64>,
        now: UtcTimestamp,
    ) -> Result<Option<FirmwareJobMutation16>, CommandErrorCode> {
        let snapshot = self
            .snapshot
            .read()
            .map_err(|_| CommandErrorCode::PolicyRejected)?;
        let Some((settings, request)) = self.firmware_request(command, &snapshot, now)? else {
            return Ok(None);
        };
        let start = request.latest_start().max(now);
        let deadline = settings
            .job_timeout
            .try_into()
            .ok()
            .and_then(|timeout| start.into_inner().checked_add(timeout))
            .map(UtcTimestamp::new)
            .ok_or(CommandErrorCode::InvalidParameters)?;
        Ok(Some(FirmwareJobMutation16 {
            station: snapshot.station.clone(),
            request_id: command.request_id.clone(),
            variant: match &request {
                Request::Legacy(_) => FirmwareVariant16::Legacy,
                Request::Signed(request) => FirmwareVariant16::Signed {
                    request_id: request.request_id,
                },
            },
            artifact_reference: request.artifact_reference().to_owned(),
            admitted_at: now,
            deadline,
            generation: generation.ok_or(CommandErrorCode::StationDisconnected)?,
        }))
    }

    /// Resolves the artifact, sends exactly one native request and maps its correlated reply.
    pub(super) async fn dispatch_firmware(
        &self,
        command: &Command<Value>,
    ) -> Result<CommandDispatchOutcome, StationCommandError> {
        let now = self.clock.now();
        let (settings, request) = {
            let snapshot = self.snapshot.read().map_err(|_| super::state_error())?;
            match self.firmware_request(command, &snapshot, now) {
                Ok(Some(found)) => found,
                Ok(None) => return Ok(mapping::not_sent(CommandErrorCode::InvalidParameters)),
                Err(code) => return Ok(mapping::not_sent(code)),
            }
        };
        let prepared = match prepare(settings, &request, now).await {
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
            SessionCallOutcome::Error { error, .. } => FirmwareReply16::CallError {
                code: FirmwareCallError16::from_code(&error.code),
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
        Ok(CommandDispatchOutcome::FirmwareResponse16(result(
            &request,
            prepared.artifact,
            reply,
            now,
        )))
    }
}

fn native_reply(request: &Request, payload: &Value) -> Option<FirmwareReply16> {
    let object = payload.as_object()?;
    match request {
        Request::Legacy(_) => object.is_empty().then_some(FirmwareReply16::Acknowledged),
        Request::Signed(_) => {
            if object.len() != 1 {
                return None;
            }
            let status: SignedUpdateFirmwareStatus16 =
                serde_json::from_value(object.get("status")?.clone()).ok()?;
            Some(FirmwareReply16::Status { status })
        }
    }
}

fn result(
    request: &Request,
    artifact: FirmwareArtifact16,
    reply: FirmwareReply16,
    now: UtcTimestamp,
) -> FirmwareResult16 {
    // Storage replaces this placeholder with the durable job revision.
    let job = FirmwareJob16 {
        revision: 0,
        state: FirmwareJobState16::Pending,
        deadline: now,
        observed_at: now,
        last_status: None,
        last_status_at: None,
        notifications: 0,
        rejected_transitions: 0,
    };
    match request {
        Request::Legacy(_) => FirmwareResult16::UpdateFirmware {
            artifact: Some(artifact),
            reply: Some(reply),
            job,
        },
        Request::Signed(signed) => FirmwareResult16::SignedUpdateFirmware {
            request_id: signed.request_id,
            artifact: Some(artifact),
            reply: Some(reply),
            job,
        },
    }
}

async fn prepare(
    settings: &FirmwareSettings16,
    request: &Request,
    now: UtcTimestamp,
) -> Result<Prepared, Refusal> {
    let unavailable = || {
        refused(
            CommandErrorCode::PolicyRejected,
            "firmware artifact unavailable",
        )
    };
    let reference = ArtifactReference::new(request.artifact_reference()).map_err(|_| {
        refused(
            CommandErrorCode::InvalidParameters,
            "invalid firmware artifact",
        )
    })?;
    let descriptor = tokio::time::timeout(PROVIDER_TIMEOUT, settings.artifacts.resolve(&reference))
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
    if settings
        .policy
        .authorize_artifact_provider(settings.artifacts.descriptor())
        .and(
            settings
                .policy
                .authorize_provider_material(descriptor.test_only()),
        )
        .is_err()
    {
        return Err(refused(
            CommandErrorCode::PolicyRejected,
            "test firmware refused in this environment",
        ));
    }
    let expected = if matches!(request, Request::Signed(_)) {
        ArtifactKind::SignedFirmware
    } else {
        ArtifactKind::Firmware
    };
    if descriptor.kind() != expected {
        return Err(refused(
            CommandErrorCode::InvalidParameters,
            "firmware artifact kind does not match the request",
        ));
    }
    let integrity = descriptor.integrity();
    let artifact = FirmwareArtifact16 {
        artifact_reference: reference.as_str().to_owned(),
        sha256: integrity.sha256.as_bytes().iter().fold(
            String::with_capacity(64),
            |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            },
        ),
        size_bytes: integrity.size_bytes,
        signed: integrity.signature.is_some(),
        test_only: descriptor.test_only(),
    };
    let location = descriptor.location().as_str();
    let payload = match request {
        Request::Legacy(legacy) => {
            let mut payload = json!({"location": location, "retrieveDate": legacy.retrieve_date});
            optional_counts(&mut payload, legacy.retries, legacy.retry_interval);
            let native: rust_ocpp::v1_6::messages::update_firmware::UpdateFirmwareRequest =
                serde_json::from_value(payload.clone()).map_err(|_| unavailable())?;
            validator::Validate::validate(&native).map_err(|_| unavailable())?;
            payload
        }
        Request::Signed(signed) => {
            let signature = integrity.signature.as_ref().ok_or_else(unavailable)?;
            verify_signer(settings, signature, now).await?;
            let mut firmware = json!({
                "location": location,
                "retrieveDateTime": signed.retrieve_date_time,
                "signingCertificate": signature.signing_certificate.as_str(),
                "signature": signature.signature.as_str(),
            });
            if let Some(install) = signed.install_date_time {
                firmware["installDateTime"] = json!(install);
            }
            let mut payload = json!({"requestId": signed.request_id, "firmware": firmware});
            optional_counts(&mut payload, signed.retries, signed.retry_interval);
            payload
        }
    };
    Ok(Prepared { payload, artifact })
}

fn optional_counts(payload: &mut Value, retries: Option<u32>, interval: Option<u32>) {
    if let Some(retries) = retries {
        payload["retries"] = json!(retries);
    }
    if let Some(interval) = interval {
        payload["retryInterval"] = json!(interval);
    }
}

/// A00.FR.706: validate the signing certificate before handing it to the station.
async fn verify_signer(
    settings: &FirmwareSettings16,
    signature: &uob_application::artifact_provider::FirmwareSignature,
    now: UtcTimestamp,
) -> Result<(), Refusal> {
    let untrusted = || {
        refused(
            CommandErrorCode::PolicyRejected,
            "firmware signing certificate is not trusted",
        )
    };
    let provider = settings.certificates.as_ref().ok_or_else(untrusted)?;
    if settings
        .policy
        .authorize_certificate_provider(provider.descriptor())
        .is_err()
    {
        return Err(untrusted());
    }
    let chain = CertificateChainPem::new(signature.signing_certificate.as_str())
        .map_err(|_| untrusted())?;
    let verification = ChainVerification {
        purpose: ChainPurpose::FirmwareSigning,
        chain,
        expected_common_name: None,
        at: now,
    };
    match tokio::time::timeout(PROVIDER_TIMEOUT, provider.verify_chain(&verification)).await {
        Ok(Ok(TrustDecision::Trusted)) => Ok(()),
        _ => Err(untrusted()),
    }
}
