use std::{error::Error, fmt};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{
    CapabilityError, ContractVersion, CorrelationId, EngineeringUnit, EventId, EventType,
    ExactDecimal, Operation, ProtocolEdition, ResourceCapabilities, ResourceRef, TransactionId,
    UtcTimestamp,
};

macro_rules! command_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates an identifier after rejecting empty or whitespace-only values.
            ///
            /// # Errors
            ///
            /// Returns [`CommandIdentityError`] when `value` has no visible characters.
            pub fn new(value: impl Into<String>) -> Result<Self, CommandIdentityError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(CommandIdentityError(stringify!($name)));
                }
                Ok(Self(value))
            }

            /// Returns the stable wire representation.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(de::Error::custom)
            }
        }
    };
}

command_id!(
    RequestId,
    "Stable idempotency identity of a command request."
);
command_id!(
    PrincipalId,
    "Authenticated principal identity assigned by an adapter."
);
command_id!(
    TargetInstanceId,
    "Stable identity of a configured target instance."
);
command_id!(ProtocolActionName, "Exact protocol-defined action name.");
command_id!(
    PayloadSchemaId,
    "Stable schema identity for a privileged protocol payload."
);

/// Failure to construct a command identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandIdentityError(&'static str);

impl fmt::Display for CommandIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} cannot be empty", self.0)
    }
}

impl Error for CommandIdentityError {}

/// Typed charging limit that does not round an exact quantity through floating point.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChargingLimit {
    /// Exact limit quantity.
    pub value: ExactDecimal,
    /// Engineering unit defining the quantity.
    pub unit: EngineeringUnit,
    /// Optional number of AC phases to which the limit applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phases: Option<u8>,
}

/// An explicitly privileged protocol action whose payload type is chosen by the adapter registry.
///
/// The generic payload prevents the canonical contract from accepting an untyped JSON object.
/// An adapter must select a known action and deserialize its payload against that action's
/// pinned schema before constructing this value.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrivilegedOcppOperation<P> {
    /// Protocol edition defining the action and payload.
    pub protocol: ProtocolEdition,
    /// Exact protocol action name selected from the adapter's supported registry.
    pub action: ProtocolActionName,
    /// Stable identity of the pinned payload schema used for validation.
    pub payload_schema: PayloadSchemaId,
    /// Statically typed payload produced by schema-aware deserialization.
    pub payload: P,
}

/// Typed operation requested through any authorized command ingress.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", content = "parameters", rename_all = "snake_case")]
pub enum CommandOperation<P> {
    /// Start charging, optionally using an authorization reference already known to the bridge.
    Start {
        /// Opaque reference to locally managed authorization context.
        #[serde(skip_serializing_if = "Option::is_none")]
        authorization_reference: Option<String>,
    },
    /// Stop one identified charging transaction.
    Stop {
        /// Canonical transaction to stop.
        transaction_id: TransactionId,
    },
    /// Set an exact charging current or power limit.
    SetChargingLimit(ChargingLimit),
    /// Execute a privileged, pinned-schema OCPP management action.
    Ocpp(PrivilegedOcppOperation<P>),
}

impl<P> CommandOperation<P> {
    /// Returns the exact capability that must be advertised before dispatch.
    #[must_use]
    pub fn required_capability(&self) -> Operation {
        match self {
            Self::Start { .. } => Operation::Start,
            Self::Stop { .. } => Operation::Stop,
            Self::SetChargingLimit(_) => Operation::SetChargingLimit,
            Self::Ocpp(operation) => Operation::ProtocolAction {
                protocol: operation.protocol,
                action: operation.action.as_str().to_owned(),
            },
        }
    }
}

/// Untrusted command fields that an adapter may deserialize from a request payload.
///
/// Authentication and target-instance context are deliberately absent. Unknown fields and
/// operation variants fail deserialization instead of being ignored or treated as extensions.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRequest<P> {
    /// Caller-supplied idempotency identity.
    pub request_id: RequestId,
    /// Optional caller-supplied correlation identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<CorrelationId>,
    /// Canonical charging resource, distinct from any selected bridge target.
    pub resource: ResourceRef,
    /// Explicit typed operation and parameters.
    pub operation: CommandOperation<P>,
    /// UTC instant after which admission or dispatch must fail.
    pub expires_at: UtcTimestamp,
}

/// Trusted identity established by an adapter's authenticated connection or mapping.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthenticatedCommandOrigin {
    /// Authenticated caller of the independent management surface.
    Management {
        /// Principal resolved from management credentials.
        principal_id: PrincipalId,
    },
    /// Authenticated caller routed through one selected target instance.
    Target {
        /// Stable configured target instance used for result routing.
        target_instance_id: TargetInstanceId,
        /// Principal resolved from the target connection or mapping configuration.
        principal_id: PrincipalId,
    },
    /// Trusted application or recovery component.
    Bridge {
        /// Stable internal component identity.
        principal_id: PrincipalId,
    },
}

/// Command submitted by an adapter after it authenticates the origin independently of payload data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalCommand<P> {
    /// Untrusted request fields accepted by the adapter.
    #[serde(flatten)]
    pub request: CommandRequest<P>,
    /// Trusted origin attached by the adapter port, never copied from the request body.
    pub origin: AuthenticatedCommandOrigin,
}

impl<P> ExternalCommand<P> {
    /// Attaches trusted adapter context to a deserialized request.
    #[must_use]
    pub const fn authenticated(
        request: CommandRequest<P>,
        origin: AuthenticatedCommandOrigin,
    ) -> Self {
        Self { request, origin }
    }

    /// Converts an authenticated external request into a durably admitted command.
    #[must_use]
    pub fn admit(self, admitted_at: UtcTimestamp) -> Command<P> {
        Command {
            schema_version: ContractVersion::V1_INITIAL,
            request_id: self.request.request_id,
            correlation_id: self.request.correlation_id,
            resource: self.request.resource,
            operation: self.request.operation,
            expires_at: self.request.expires_at,
            origin: self.origin,
            admitted_at,
        }
    }
}

/// Durable canonical command admitted through any authorized ingress.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct Command<P> {
    /// Version of this command contract.
    pub schema_version: ContractVersion,
    /// Stable idempotency identity.
    pub request_id: RequestId,
    /// Correlation identity shared with related requests, results, and events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<CorrelationId>,
    /// Canonical charging resource; never the selected output target identity.
    pub resource: ResourceRef,
    /// Explicit typed operation and parameters.
    pub operation: CommandOperation<P>,
    /// UTC instant after which the command must not be admitted or dispatched.
    pub expires_at: UtcTimestamp,
    /// Authenticated origin and return-routing context supplied by an adapter port.
    pub origin: AuthenticatedCommandOrigin,
    /// UTC instant at which durable admission completed.
    pub admitted_at: UtcTimestamp,
}

impl<P> Command<P> {
    /// Verifies that a command is unexpired and its exact operation is advertised.
    ///
    /// # Errors
    ///
    /// Returns [`CommandValidationError::Expired`] at or after the expiry instant, or
    /// [`CommandValidationError::UnsupportedOperation`] when capability was not explicit.
    pub fn validate_for_dispatch(
        &self,
        capabilities: &ResourceCapabilities,
        now: UtcTimestamp,
    ) -> Result<(), CommandValidationError> {
        if now >= self.expires_at {
            return Err(CommandValidationError::Expired);
        }
        capabilities
            .require(&self.operation.required_capability())
            .map(|_| ())
            .map_err(CommandValidationError::UnsupportedOperation)
    }

    /// Returns immutable result-routing context for this request.
    #[must_use]
    pub fn return_route(&self) -> CommandReturnRoute {
        CommandReturnRoute {
            request_id: self.request_id.clone(),
            origin: self.origin.clone(),
        }
    }
}

/// Failure to validate a command before dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandValidationError {
    /// The command deadline has elapsed.
    Expired,
    /// The targeted resource did not explicitly advertise the exact operation.
    UnsupportedOperation(CapabilityError),
}

impl fmt::Display for CommandValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Expired => formatter.write_str("command has expired"),
            Self::UnsupportedOperation(error) => error.fmt(formatter),
        }
    }
}

impl Error for CommandValidationError {}

/// Immutable routing context carried by every command result.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommandReturnRoute {
    /// Request whose origin receives the result.
    pub request_id: RequestId,
    /// Authenticated management or target context, including target instance when applicable.
    pub origin: AuthenticatedCommandOrigin,
}

/// Stable, machine-readable command error.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommandError {
    /// Stable error category for automation and diagnostics.
    pub code: CommandErrorCode,
    /// Sanitized detail that must not contain credentials or untrusted raw payloads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Stable categories for command admission and execution failures.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandErrorCode {
    /// Authenticated principal lacks permission for the operation or resource.
    Unauthorized,
    /// Command parameters failed validation.
    InvalidParameters,
    /// Command expired before admission or dispatch.
    Expired,
    /// Resource does not explicitly advertise the requested operation.
    UnsupportedOperation,
    /// Target station is disconnected.
    StationDisconnected,
    /// Safety or local policy rejected the operation.
    PolicyRejected,
    /// Charger returned a protocol-level rejection or error.
    ProtocolRejected,
}

/// Lifecycle stage and outcome of a command, without conflating later observed effects.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum CommandLifecycle {
    /// Command was durably accepted for execution.
    Admitted,
    /// Command was rejected before durable admission or dispatch.
    Rejected {
        /// Stable rejection reason.
        error: CommandError,
    },
    /// Command was dispatched to the station and awaits a response.
    Dispatched,
    /// Charger supplied a protocol response; this is not proof of physical effect.
    ProtocolResponse {
        /// Whether the charger accepted the requested protocol operation.
        accepted: bool,
        /// Stable error when the protocol response rejected the operation.
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<CommandError>,
    },
    /// Transmission may have occurred, but retrying could duplicate a non-idempotent action.
    TransmissionUncertain {
        /// Sanitized reason for the uncertain outcome.
        detail: String,
    },
}

/// Separately linked evidence of a later observed charging effect.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ObservedCommandEffect {
    /// Durable event that records the observed effect.
    pub event_id: EventId,
    /// Semantic type of the observed effect.
    pub event_type: EventType,
    /// UTC instant at which the bridge observed it.
    pub observed_at: UtcTimestamp,
}

/// Versioned command result returned to its authenticated origin and available to observers.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommandResult {
    /// Version of this result contract.
    pub schema_version: ContractVersion,
    /// Request/result correlation identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<CorrelationId>,
    /// Canonical charging resource targeted by the request.
    pub resource: ResourceRef,
    /// Immutable route back to the authenticated origin and target instance.
    pub return_route: CommandReturnRoute,
    /// Latest command lifecycle stage.
    pub lifecycle: CommandLifecycle,
    /// UTC instant at which this result state was recorded.
    pub recorded_at: UtcTimestamp,
    /// Later observed effects, linked independently from charger protocol acceptance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed_effects: Vec<ObservedCommandEffect>,
}
