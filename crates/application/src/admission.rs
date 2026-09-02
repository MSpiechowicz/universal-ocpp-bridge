use std::{error::Error, fmt, future::Future, pin::Pin};

use uob_contracts::{CommandResult, ExternalCommand};

/// Future returned by the application-owned command admission boundary.
pub type CommandAdmissionFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, CommandAdmissionError>> + Send + 'a>>;

/// Stable failures returned by the common command authorization and admission path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandAdmissionErrorCode {
    /// The authenticated origin lacks permission for the operation or resource.
    Unauthorized,
    /// The command expired before durable admission.
    Expired,
    /// The resource does not advertise the requested operation.
    Unsupported,
    /// Safety or local policy rejected the command.
    PolicyRejected,
    /// The bounded admission queue cannot currently accept work.
    Busy,
    /// Authoritative storage cannot safely admit another charging session.
    StorageCapacityExhausted,
    /// Authoritative state is temporarily unavailable.
    Unavailable,
    /// The request is malformed or conflicts with an existing request identity.
    InvalidRequest,
}

/// Sanitized command admission failure with no adapter or storage source chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandAdmissionError {
    code: CommandAdmissionErrorCode,
    context: String,
}

impl CommandAdmissionError {
    /// Creates an error from a stable code and pre-sanitized context.
    #[must_use]
    pub fn new(code: CommandAdmissionErrorCode, context: impl Into<String>) -> Self {
        Self {
            code,
            context: context.into(),
        }
    }

    /// Returns the stable machine-readable category.
    #[must_use]
    pub const fn code(&self) -> CommandAdmissionErrorCode {
        self.code
    }

    /// Returns bounded sanitized context.
    #[must_use]
    pub fn context(&self) -> &str {
        &self.context
    }
}

impl fmt::Display for CommandAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "command admission {:?}: {}",
            self.code, self.context
        )
    }
}

impl Error for CommandAdmissionError {}

/// Common host-owned command ingress used by management, targets, and verified providers.
///
/// Implementations reapply authorization, capability, safety, expiry, and idempotency policy
/// before durable admission. An outward adapter never receives storage or charger-socket access.
pub trait CommandAdmissionPort<P>: Send + Sync {
    /// Submits one command with origin context established outside its request payload.
    fn submit(&self, command: ExternalCommand<P>) -> CommandAdmissionFuture<'_, CommandResult>;
}
