//! Protected OCPP 2.0.1 `UpdateFirmware` (L01 secure, L02 non-secure). The public request
//! names an artifact; the provider supplies what the station needs at the send boundary.
#[cfg(test)]
#[path = "firmware_tests.rs"]
mod tests;
use super::{RemoteControlSession, ReservationGrant201, mapping};
use crate::SessionCallOutcome;
use crate::command_registry::firmware201;
use serde_json::{Map, Value, json};
use std::{fmt::Write, sync::Arc, time::Duration};
use uob_application::{
    CommandDispatchOutcome, FirmwareJobMutation201, RuntimeSecurityPolicy,
    artifact_provider::{ArtifactKind, ArtifactProvider, ArtifactReference, FirmwareSignature},
    certificate_provider::{
        CertificateChainPem, CertificateProvider, ChainPurpose, ChainVerification, TrustDecision,
    },
};
use uob_contracts::{
    Command, CommandError, CommandErrorCode, CommandOperation, FirmwareArtifact201,
    FirmwareCallError201, FirmwareJob201, FirmwareJobState201, FirmwareReply201, FirmwareResult201,
    ProtocolEdition, StationSnapshot, UpdateFirmwareReference201, UpdateFirmwareStatus201,
    UtcTimestamp,
};

/// Longest provider or PKI call before the request is refused without transmission.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(10);

/// Station-scoped firmware policy installed only for an explicitly enabled station.
pub struct FirmwareSettings201 {
    /// Secure update (L01): the request carries a verified signing certificate and signature.
    /// Otherwise the station receives a non-secure update (L02) with neither.
    pub secure: bool,
    /// Bridge deadline added to the latest native start instant.
    pub job_timeout: Duration,
    pub artifacts: Arc<dyn ArtifactProvider>,
    /// Required for secure updates; verifies the signing certificate before sending.
    pub certificates: Option<Arc<dyn CertificateProvider>>,
    pub policy: RuntimeSecurityPolicy,
    pub grant: Arc<ReservationGrant201>,
}

/// Whether this module owns the command's dispatch.
pub(super) fn owns(command: &Command<Value>) -> bool {
    matches!(&command.operation, CommandOperation::Ocpp(operation)
        if operation.protocol == ProtocolEdition::Ocpp201
            && operation.action.as_str() == firmware201::ACTION)
}

struct Prepared {
    payload: Value,
    artifact: FirmwareArtifact201,
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
    /// Enables protected firmware updates for this authenticated socket only.
    #[must_use]
    pub fn with_firmware_201(mut self, settings: Arc<FirmwareSettings201>) -> Self {
        self.firmware = Some(settings);
        self
    }

    fn firmware_request(
        &self,
        command: &Command<Value>,
        snapshot: &StationSnapshot,
        now: UtcTimestamp,
    ) -> Result<Option<(&FirmwareSettings201, UpdateFirmwareReference201)>, CommandErrorCode> {
        let CommandOperation::Ocpp(operation) = &command.operation else {
            return Ok(None);
        };
        if !owns(command) {
            return Ok(None);
        }
        let settings = self
            .firmware
            .as_deref()
            .ok_or(CommandErrorCode::UnsupportedOperation)?;
        let request = firmware201::validate(&command.resource, operation)?;
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
    pub(super) fn firmware_context(
        &self,
        command: &Command<Value>,
        generation: Option<u64>,
        now: UtcTimestamp,
    ) -> Result<Option<FirmwareJobMutation201>, CommandErrorCode> {
        let snapshot = self
            .snapshot
            .read()
            .map_err(|_| CommandErrorCode::PolicyRejected)?;
        let Some((settings, request)) = self.firmware_request(command, &snapshot, now)? else {
            return Ok(None);
        };
        let start = firmware201::latest_start(&request).max(now);
        let deadline = settings
            .job_timeout
            .try_into()
            .ok()
            .and_then(|timeout| start.into_inner().checked_add(timeout))
            .map(UtcTimestamp::new)
            .ok_or(CommandErrorCode::InvalidParameters)?;
        Ok(Some(FirmwareJobMutation201 {
            station: snapshot.station.clone(),
            request_id: command.request_id.clone(),
            native_request_id: request.request_id,
            secure: settings.secure,
            artifact_reference: request.artifact_reference,
            admitted_at: now,
            deadline,
            generation: generation.ok_or(CommandErrorCode::StationDisconnected)?,
        }))
    }

    /// Resolves the artifact, sends exactly one native request and maps its correlated reply.
    pub(super) async fn dispatch_firmware(
        &self,
        command: &Command<Value>,
    ) -> CommandDispatchOutcome {
        let now = self.clock.now();
        let checked = match self.snapshot.read() {
            Ok(snapshot) => self.firmware_request(command, &snapshot, now),
            Err(_) => Err(CommandErrorCode::PolicyRejected),
        };
        let (settings, request) = match checked {
            Ok(Some(found)) => found,
            Ok(None) => return mapping::not_sent(CommandErrorCode::InvalidParameters),
            Err(code) => return mapping::not_sent(code),
        };
        let prepared = match prepare(settings, &request, now).await {
            Ok(prepared) => prepared,
            Err(refusal) => return refusal.into(),
        };
        // Recheck authority after the provider calls, under the snapshot lock.
        let pending = match self.enqueue_rechecked(
            command,
            firmware201::ACTION,
            prepared.payload,
            |snapshot, now| self.firmware_request(command, snapshot, now).map(|_| ()),
        ) {
            Ok(pending) => pending,
            Err(code) => return mapping::not_sent(code),
        };
        let reply = match pending.receive().await {
            SessionCallOutcome::Result { payload, .. } => match native_reply(&payload) {
                Some(reply) => reply,
                None => return mapping::uncertain(),
            },
            SessionCallOutcome::Error { error, .. } => FirmwareReply201::CallError {
                code: FirmwareCallError201::from_code(&error.code),
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
        CommandDispatchOutcome::FirmwareResponse201(FirmwareResult201 {
            request_id: request.request_id,
            secure: settings.secure,
            artifact: Some(prepared.artifact),
            reply: Some(reply),
            job: FirmwareJob201 {
                revision: 0,
                state: FirmwareJobState201::Pending,
                deadline: now,
                observed_at: now,
                last_status: None,
                last_status_at: None,
                notifications: 0,
                rejected_transitions: 0,
            },
        })
    }
}

/// Exact `UpdateFirmwareResponse`; anything else leaves delivery uncertain.
pub(crate) fn native_reply(payload: &Value) -> Option<FirmwareReply201> {
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
    if !only(object, &["status", "statusInfo", "customData"]) {
        return None;
    }
    let status: UpdateFirmwareStatus201 =
        serde_json::from_value(object.get("status")?.clone()).ok()?;
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
            if !uob_contracts::valid_firmware_reason_code_201(code) {
                return None;
            }
            Some(code.to_owned())
        }
    };
    Some(FirmwareReply201::Status {
        status,
        reason_code,
    })
}

async fn prepare(
    settings: &FirmwareSettings201,
    request: &UpdateFirmwareReference201,
    now: UtcTimestamp,
) -> Result<Prepared, Refusal> {
    let unavailable = || {
        refused(
            CommandErrorCode::PolicyRejected,
            "firmware artifact unavailable",
        )
    };
    let reference = ArtifactReference::new(request.artifact_reference.as_str()).map_err(|_| {
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
    let expected = if settings.secure {
        ArtifactKind::SignedFirmware
    } else {
        ArtifactKind::Firmware
    };
    if descriptor.kind() != expected {
        return Err(refused(
            CommandErrorCode::InvalidParameters,
            "firmware artifact kind does not match the station",
        ));
    }
    let integrity = descriptor.integrity();
    let mut firmware = json!({
        "location": descriptor.location().as_str(),
        "retrieveDateTime": request.retrieve_date_time,
    });
    if let Some(install) = request.install_date_time {
        firmware["installDateTime"] = json!(install);
    }
    if settings.secure {
        // L01.FR.11: a secure request carries the signing certificate and signature.
        let signature = integrity.signature.as_ref().ok_or_else(unavailable)?;
        verify_signer(settings, signature, now).await?;
        firmware["signingCertificate"] = json!(signature.signing_certificate.as_str());
        firmware["signature"] = json!(signature.signature.as_str());
    }
    let mut payload = json!({"requestId": request.request_id, "firmware": firmware});
    if let Some(retries) = request.retries {
        payload["retries"] = json!(retries);
    }
    if let Some(interval) = request.retry_interval {
        payload["retryInterval"] = json!(interval);
    }
    serde_json::from_value::<rust_ocpp::v2_0_1::messages::update_firmware::UpdateFirmwareRequest>(
        payload.clone(),
    )
    .map_err(|_| unavailable())?;
    let artifact = FirmwareArtifact201 {
        artifact_reference: reference.as_str().to_owned(),
        sha256: integrity.sha256.as_bytes().iter().fold(
            String::with_capacity(64),
            |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            },
        ),
        size_bytes: integrity.size_bytes,
        signed: settings.secure,
        test_only: descriptor.test_only(),
    };
    Ok(Prepared { payload, artifact })
}

/// Validates the signing certificate under the manufacturer root before handing it to the
/// station, so a station never has to refuse it (L01.FR.21/22).
async fn verify_signer(
    settings: &FirmwareSettings201,
    signature: &FirmwareSignature,
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
