use serde::de::DeserializeOwned;
use time::OffsetDateTime;
use uob_application::{CommandAdmissionError, CommandAdmissionErrorCode};
use uob_contracts::{
    AuthenticatedCommandOrigin, CommandError, CommandErrorCode, CommandLifecycle, CommandOperation,
    CommandRequest, CommandResult, ContractVersion, CorrelationId, ExternalCommand, PrincipalId,
    RequestId, ResourceRef, TargetInstanceId, UtcTimestamp,
};

use crate::mapping::TopicNamespace;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandPayload<P> {
    schema_version: ContractVersion,
    request_id: RequestId,
    #[serde(default)]
    correlation_id: Option<CorrelationId>,
    resource: ResourceRef,
    operation: CommandOperation<P>,
    expires_at: UtcTimestamp,
}

pub(crate) struct Ingress<P> {
    command: Option<ExternalCommand<P>>,
    alternate: Option<IngressAlternate>,
}

pub(crate) enum IngressAlternate {
    Reject(Box<CommandResult>),
    Ignore(&'static str),
}

impl<P> Ingress<P> {
    fn submit(command: ExternalCommand<P>) -> Self {
        Self {
            command: Some(command),
            alternate: None,
        }
    }

    fn reject(result: CommandResult) -> Self {
        Self {
            command: None,
            alternate: Some(IngressAlternate::Reject(Box::new(result))),
        }
    }

    fn ignore(reason: &'static str) -> Self {
        Self {
            command: None,
            alternate: Some(IngressAlternate::Ignore(reason)),
        }
    }

    pub(crate) fn into_result(self) -> Result<ExternalCommand<P>, IngressAlternate> {
        match (self.command, self.alternate) {
            (Some(command), None) => Ok(command),
            (None, Some(alternate)) => Err(alternate),
            _ => unreachable!("ingress must have exactly one outcome"),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct IngressContext<'a> {
    pub(crate) topics: &'a TopicNamespace,
    pub(crate) target_instance_id: &'a TargetInstanceId,
    pub(crate) principal: &'a PrincipalId,
    pub(crate) maximum_command_bytes: usize,
}

pub(crate) fn classify<P: DeserializeOwned>(
    context: IngressContext<'_>,
    topic: &str,
    payload: &[u8],
    retained: bool,
) -> Ingress<P> {
    if payload.len() > context.maximum_command_bytes {
        return Ingress::ignore("mqtt.command_too_large");
    }
    let Ok(payload) = serde_json::from_slice::<CommandPayload<P>>(payload) else {
        return Ingress::ignore("mqtt.command_invalid");
    };
    if payload.schema_version != ContractVersion::V1_INITIAL {
        return Ingress::ignore("mqtt.command_version_unsupported");
    }
    let request = CommandRequest {
        request_id: payload.request_id,
        correlation_id: payload.correlation_id,
        resource: payload.resource,
        operation: payload.operation,
        expires_at: payload.expires_at,
    };
    let external = ExternalCommand::authenticated(
        request,
        AuthenticatedCommandOrigin::Target {
            target_instance_id: context.target_instance_id.clone(),
            principal_id: context.principal.clone(),
        },
    );
    let now = UtcTimestamp::new(OffsetDateTime::now_utc());
    if !context.topics.command_topic_matches(
        topic,
        &external.request.resource,
        &external.request.request_id,
    ) {
        return Ingress::reject(rejected(
            &external,
            CommandErrorCode::Unauthorized,
            "mqtt.command_scope_mismatch",
            now,
        ));
    }
    if retained {
        return Ingress::reject(rejected(
            &external,
            CommandErrorCode::PolicyRejected,
            "mqtt.retained_command",
            now,
        ));
    }
    if now >= external.request.expires_at {
        return Ingress::reject(rejected(
            &external,
            CommandErrorCode::Expired,
            "mqtt.command_expired",
            now,
        ));
    }
    Ingress::submit(external)
}

pub(crate) fn admission_rejection<P>(
    command: &ExternalCommand<P>,
    error: &CommandAdmissionError,
) -> CommandResult {
    let (code, detail) = match error.code() {
        CommandAdmissionErrorCode::Unauthorized => {
            (CommandErrorCode::Unauthorized, "mqtt.command_unauthorized")
        }
        CommandAdmissionErrorCode::Expired => (CommandErrorCode::Expired, "mqtt.command_expired"),
        CommandAdmissionErrorCode::Unsupported => (
            CommandErrorCode::UnsupportedOperation,
            "mqtt.command_unsupported",
        ),
        CommandAdmissionErrorCode::PolicyRejected => (
            CommandErrorCode::PolicyRejected,
            "mqtt.command_policy_rejected",
        ),
        CommandAdmissionErrorCode::InvalidRequest => (
            CommandErrorCode::InvalidParameters,
            "mqtt.command_request_conflict",
        ),
        CommandAdmissionErrorCode::Busy
        | CommandAdmissionErrorCode::StorageCapacityExhausted
        | CommandAdmissionErrorCode::Unavailable => (
            CommandErrorCode::PolicyRejected,
            "mqtt.command_temporarily_unavailable",
        ),
    };
    rejected(
        command,
        code,
        detail,
        UtcTimestamp::new(OffsetDateTime::now_utc()),
    )
}

fn rejected<P>(
    command: &ExternalCommand<P>,
    code: CommandErrorCode,
    detail: &'static str,
    recorded_at: UtcTimestamp,
) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: command.request.correlation_id.clone(),
        resource: command.request.resource.clone(),
        return_route: uob_contracts::CommandReturnRoute {
            request_id: command.request.request_id.clone(),
            origin: command.origin.clone(),
        },
        lifecycle: CommandLifecycle::Rejected {
            error: CommandError {
                code,
                detail: Some(detail.to_owned()),
            },
        },
        recorded_at,
        observed_effects: Vec::new(),
        configuration: None,
        configuration_observations: Vec::new(),
        trigger_observation: None,
        trigger_observation_201: None,
    }
}
