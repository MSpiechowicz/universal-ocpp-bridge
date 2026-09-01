//! Payment orchestration boundaries independent of browser and target transports.

use std::{error::Error, fmt, future::Future, pin::Pin, sync::Arc};

use uob_contracts::{
    AuthenticatedCommandOrigin, CommandRequest, CommandResult, ExternalCommand, PrincipalId,
    RequestId, ResourceRef, UtcTimestamp,
};

use crate::{CommandAdmissionError, CommandAdmissionPort};

/// Future returned by payment provider, intent, and audit ports.
pub type PaymentFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PaymentError>> + Send + 'a>>;

macro_rules! payment_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// Creates a non-empty bounded payment identity.
            ///
            /// # Errors
            ///
            /// Returns [`PaymentErrorCode::InvalidRequest`] for empty or oversized values.
            pub fn new(value: impl Into<String>) -> Result<Self, PaymentError> {
                let value = value.into();
                if value.trim().is_empty() || value.len() > 256 {
                    return Err(PaymentError::new(
                        PaymentErrorCode::InvalidRequest,
                        concat!(stringify!($name), ".invalid"),
                    ));
                }
                Ok(Self(value))
            }

            /// Returns the stable identity.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.0)
                    .finish()
            }
        }
    };
}

payment_id!(
    PaymentProviderId,
    "Stable configured payment provider identity."
);
payment_id!(
    CheckoutIntentId,
    "Stable idempotency identity of one checkout intent."
);

/// Provider-only checkout or callback bytes.
///
/// This value deliberately implements neither `Serialize` nor `Clone`. Its `Debug` output is
/// always redacted, and only payment adapter code should inspect the bytes.
pub struct SensitivePaymentData(Vec<u8>);

impl SensitivePaymentData {
    /// Wraps provider-specific data without interpreting or logging it.
    #[must_use]
    pub fn new(value: impl Into<Vec<u8>>) -> Self {
        Self(value.into())
    }

    /// Exposes bytes only at the payment provider/adapter boundary.
    #[must_use]
    pub fn expose_to_payment_provider(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SensitivePaymentData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitivePaymentData([REDACTED])")
    }
}

/// Safe command binding retained by the application while checkout is pending.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckoutIntent<P> {
    /// Stable checkout identity used for provider-event correlation.
    pub intent_id: CheckoutIntentId,
    /// Command to submit only after provider verification succeeds.
    pub command: CommandRequest<P>,
}

impl<P> CheckoutIntent<P> {
    /// Returns the canonical charging resource bound to the checkout.
    #[must_use]
    pub const fn resource(&self) -> &ResourceRef {
        &self.command.resource
    }
}

/// Provider checkout request. Sensitive fields never enter the intent store.
#[derive(Debug)]
pub struct CheckoutRequest<P> {
    /// Safe application-owned command binding.
    pub intent: CheckoutIntent<P>,
    /// Provider-specific checkout details, credentials, or payment method data.
    pub provider_data: SensitivePaymentData,
}

/// Provider presentation data returned to a scoped checkout client.
#[derive(Debug)]
pub struct CheckoutPresentation {
    /// Provider-specific redirect/client data treated as payment-sensitive.
    pub client_data: SensitivePaymentData,
    /// Checkout expiry established by the provider.
    pub expires_at: UtcTimestamp,
}

/// Raw callback data delivered only to the configured payment provider.
#[derive(Debug)]
pub struct PaymentProviderEvent {
    /// Provider-specific signed callback or equivalent verification material.
    pub provider_data: SensitivePaymentData,
}

/// Untrusted authorization input accepted at the orchestration boundary.
#[derive(Debug)]
pub enum PaymentAuthorizationInput {
    /// Raw provider callback that still requires configured-provider verification.
    ProviderEvent(PaymentProviderEvent),
    /// Browser/WebView assertion, which can never prove payment.
    BrowserAssertion {
        /// Claimed checkout identity used only for a sanitized rejection audit.
        intent_id: CheckoutIntentId,
        /// Untrusted claimed success value; both values are rejected.
        succeeded: bool,
    },
    /// Selected-target assertion, which can never prove payment.
    TargetAssertion {
        /// Claimed checkout identity used only for a sanitized rejection audit.
        intent_id: CheckoutIntentId,
        /// Untrusted claimed success value; both values are rejected.
        succeeded: bool,
    },
}

/// Opaque safe reference to verification evidence retained by the provider.
#[derive(Clone, Eq, PartialEq)]
pub struct PaymentVerificationReference(String);

impl PaymentVerificationReference {
    /// Creates a non-empty bounded verification reference.
    ///
    /// # Errors
    ///
    /// Returns [`PaymentErrorCode::InvalidProviderResponse`] for an invalid reference.
    pub fn new(value: impl Into<String>) -> Result<Self, PaymentError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 256 {
            return Err(PaymentError::new(
                PaymentErrorCode::InvalidProviderResponse,
                "verification_reference.invalid",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the provider-owned opaque reference.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PaymentVerificationReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PaymentVerificationReference([OPAQUE])")
    }
}

/// Proof returned by the configured provider after it validates a raw event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedPaymentEvent {
    provider_id: PaymentProviderId,
    intent_id: CheckoutIntentId,
    verification_reference: PaymentVerificationReference,
    verified_at: UtcTimestamp,
}

impl VerifiedPaymentEvent {
    /// Creates provider verification evidence.
    ///
    /// This constructor is for implementations of [`PaymentProvider`]. The orchestrator never
    /// accepts this proof directly from browser or target callers.
    #[must_use]
    pub const fn new(
        provider_id: PaymentProviderId,
        intent_id: CheckoutIntentId,
        verification_reference: PaymentVerificationReference,
        verified_at: UtcTimestamp,
    ) -> Self {
        Self {
            provider_id,
            intent_id,
            verification_reference,
            verified_at,
        }
    }

    /// Returns the verified checkout identity.
    #[must_use]
    pub const fn intent_id(&self) -> &CheckoutIntentId {
        &self.intent_id
    }
}

/// Payment provider port selected independently of the bridge target.
pub trait PaymentProvider<P>: Send + Sync {
    /// Returns the stable configured provider identity.
    fn provider_id(&self) -> &PaymentProviderId;

    /// Creates provider presentation data for an application-owned checkout binding.
    fn create_checkout(
        &self,
        request: &CheckoutRequest<P>,
    ) -> PaymentFuture<'_, CheckoutPresentation>;

    /// Verifies raw provider data and returns evidence only for a successful payment.
    fn verify_event(&self, event: PaymentProviderEvent) -> PaymentFuture<'_, VerifiedPaymentEvent>;
}

/// Durable application-owned checkout binding and single-use verification claim port.
pub trait PaymentIntentStore<P>: Send + Sync {
    /// Saves a pending checkout without any provider payment details.
    fn save(&self, intent: &CheckoutIntent<P>) -> PaymentFuture<'_, ()>;

    /// Atomically claims the command bound to verified evidence exactly once.
    fn claim_verified(
        &self,
        verification: VerifiedPaymentEvent,
    ) -> PaymentFuture<'_, CheckoutIntent<P>>;
}

/// Safe auditable fact recorded before a verified payment can submit its command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaymentAuthorizationAudit {
    /// Configured provider that performed verification.
    pub provider_id: PaymentProviderId,
    /// Checkout binding claimed by the verified event.
    pub intent_id: CheckoutIntentId,
    /// Provider-owned evidence reference; its debug representation remains opaque.
    pub verification_reference: PaymentVerificationReference,
    /// Correlated command request submitted through ordinary admission.
    pub request_id: RequestId,
    /// Provider-established verification time.
    pub verified_at: UtcTimestamp,
}

/// Durable audit sink isolated from default target events, exports, and diagnostics.
pub trait PaymentAuditPort: Send + Sync {
    /// Records safe verification provenance before command submission.
    fn record_authorization(&self, audit: PaymentAuthorizationAudit) -> PaymentFuture<'_, ()>;
}

/// Application-owned payment coordinator with no browser, target, or provider SDK types.
pub struct PaymentOrchestrator<P> {
    provider: Arc<dyn PaymentProvider<P>>,
    intents: Arc<dyn PaymentIntentStore<P>>,
    admissions: Arc<dyn CommandAdmissionPort<P>>,
    audit: Arc<dyn PaymentAuditPort>,
    payment_principal: PrincipalId,
}

impl<P: Send + 'static> PaymentOrchestrator<P> {
    /// Wires a configured provider to application-owned intent, audit, and command ports.
    #[must_use]
    pub fn new(
        provider: Arc<dyn PaymentProvider<P>>,
        intents: Arc<dyn PaymentIntentStore<P>>,
        admissions: Arc<dyn CommandAdmissionPort<P>>,
        audit: Arc<dyn PaymentAuditPort>,
        payment_principal: PrincipalId,
    ) -> Self {
        Self {
            provider,
            intents,
            admissions,
            audit,
            payment_principal,
        }
    }

    /// Creates provider checkout data and retains only its safe command binding.
    ///
    /// # Errors
    ///
    /// Returns a sanitized provider or intent-store failure.
    pub async fn begin_checkout(
        &self,
        request: CheckoutRequest<P>,
    ) -> Result<CheckoutPresentation, PaymentError> {
        self.intents.save(&request.intent).await?;
        let presentation = self.provider.create_checkout(&request).await?;
        Ok(presentation)
    }

    /// Verifies provider input and submits its bound command through ordinary admission.
    ///
    /// Browser and target assertions are rejected before the provider, intent store, audit, or
    /// admission port is called.
    ///
    /// # Errors
    ///
    /// Returns [`PaymentErrorCode::UnverifiedSource`] for browser/target assertions, or a
    /// sanitized provider, intent, audit, or command-admission failure.
    pub async fn authorize(
        &self,
        input: PaymentAuthorizationInput,
    ) -> Result<CommandResult, PaymentError> {
        let event = match input {
            PaymentAuthorizationInput::ProviderEvent(event) => event,
            PaymentAuthorizationInput::BrowserAssertion { .. }
            | PaymentAuthorizationInput::TargetAssertion { .. } => {
                return Err(PaymentError::new(
                    PaymentErrorCode::UnverifiedSource,
                    "payment.assertion_untrusted",
                ));
            }
        };

        let verification = self.provider.verify_event(event).await?;
        if verification.provider_id != *self.provider.provider_id() {
            return Err(PaymentError::new(
                PaymentErrorCode::InvalidProviderResponse,
                "payment.provider_identity_mismatch",
            ));
        }
        let intent = self.intents.claim_verified(verification.clone()).await?;
        if intent.intent_id != verification.intent_id {
            return Err(PaymentError::new(
                PaymentErrorCode::InvalidProviderResponse,
                "payment.intent_identity_mismatch",
            ));
        }

        self.audit
            .record_authorization(PaymentAuthorizationAudit {
                provider_id: verification.provider_id,
                intent_id: verification.intent_id,
                verification_reference: verification.verification_reference,
                request_id: intent.command.request_id.clone(),
                verified_at: verification.verified_at,
            })
            .await?;

        let origin = AuthenticatedCommandOrigin::Bridge {
            principal_id: self.payment_principal.clone(),
        };
        self.admissions
            .submit(ExternalCommand::authenticated(intent.command, origin))
            .await
            .map_err(PaymentError::from)
    }
}

/// Stable payment boundary failure categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaymentErrorCode {
    /// A caller supplied an invalid identity or checkout request.
    InvalidRequest,
    /// Browser or target data attempted to self-declare payment success.
    UnverifiedSource,
    /// Configured provider verification rejected or could not validate the event.
    VerificationFailed,
    /// Provider output did not match its configured identity or pending checkout.
    InvalidProviderResponse,
    /// The checkout is unknown, expired, conflicting, or already claimed.
    IntentUnavailable,
    /// Provider or authoritative application state is temporarily unavailable.
    Unavailable,
    /// A safe verification audit could not be recorded.
    AuditFailed,
    /// The ordinary command authorization/admission path rejected the request.
    CommandRejected,
}

/// Sanitized payment error which never stores provider payloads or payment details.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaymentError {
    code: PaymentErrorCode,
    context: String,
}

impl PaymentError {
    /// Creates an error from a stable code and pre-sanitized context.
    #[must_use]
    pub fn new(code: PaymentErrorCode, context: impl Into<String>) -> Self {
        Self {
            code,
            context: context.into(),
        }
    }

    /// Returns the stable machine-readable category.
    #[must_use]
    pub const fn code(&self) -> PaymentErrorCode {
        self.code
    }

    /// Returns bounded sanitized context.
    #[must_use]
    pub fn context(&self) -> &str {
        &self.context
    }
}

impl From<CommandAdmissionError> for PaymentError {
    fn from(error: CommandAdmissionError) -> Self {
        Self::new(
            PaymentErrorCode::CommandRejected,
            format!("command.{:?}", error.code()),
        )
    }
}

impl fmt::Display for PaymentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "payment {:?}: {}", self.code, self.context)
    }
}

impl Error for PaymentError {}
