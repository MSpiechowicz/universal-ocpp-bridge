use std::{
    future::Future,
    pin::pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_application::{
    CheckoutIntent, CheckoutIntentId, CheckoutPresentation, CheckoutRequest,
    CommandAdmissionFuture, CommandAdmissionPort, PaymentAuditPort, PaymentAuthorizationAudit,
    PaymentAuthorizationInput, PaymentError, PaymentErrorCode, PaymentFuture, PaymentIntentStore,
    PaymentOrchestrator, PaymentProvider, PaymentProviderEvent, PaymentProviderId,
    PaymentVerificationReference, SensitivePaymentData, VerifiedPaymentEvent,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, BridgeId, CommandLifecycle, CommandOperation, CommandRequest,
    CommandResult, CommandReturnRoute, ContractVersion, ExternalCommand, PrincipalId, RequestId,
    ResourceRef, StationId, UtcTimestamp,
};

#[derive(Default)]
struct Calls {
    create: usize,
    verify: usize,
    claim: usize,
    audit: usize,
    admit: usize,
}

struct FakeProvider {
    provider_id: PaymentProviderId,
    verified_intent: CheckoutIntentId,
    calls: Arc<Mutex<Calls>>,
}

impl PaymentProvider<()> for FakeProvider {
    fn provider_id(&self) -> &PaymentProviderId {
        &self.provider_id
    }

    fn create_checkout(
        &self,
        request: &CheckoutRequest<()>,
    ) -> PaymentFuture<'_, CheckoutPresentation> {
        let details = request.provider_data.expose_to_payment_provider();
        assert_eq!(details, b"card-number=not-for-default-surfaces");
        self.calls.lock().expect("calls").create += 1;
        Box::pin(async {
            Ok(CheckoutPresentation {
                client_data: SensitivePaymentData::new("provider-client-secret"),
                expires_at: timestamp(8),
            })
        })
    }

    fn verify_event(&self, event: PaymentProviderEvent) -> PaymentFuture<'_, VerifiedPaymentEvent> {
        let valid = event.provider_data.expose_to_payment_provider() == b"signed-provider-event";
        self.calls.lock().expect("calls").verify += 1;
        let provider_id = self.provider_id.clone();
        let intent_id = self.verified_intent.clone();
        Box::pin(async move {
            if !valid {
                return Err(PaymentError::new(
                    PaymentErrorCode::VerificationFailed,
                    "provider.signature_invalid",
                ));
            }
            Ok(VerifiedPaymentEvent::new(
                provider_id,
                intent_id,
                PaymentVerificationReference::new("provider-evidence-42")?,
                timestamp(2),
            ))
        })
    }
}

struct MemoryIntents {
    pending: Mutex<Option<CheckoutIntent<()>>>,
    calls: Arc<Mutex<Calls>>,
}

impl PaymentIntentStore<()> for MemoryIntents {
    fn save(&self, intent: &CheckoutIntent<()>) -> PaymentFuture<'_, ()> {
        *self.pending.lock().expect("pending") = Some(intent.clone());
        Box::pin(async { Ok(()) })
    }

    fn claim_verified(
        &self,
        verification: VerifiedPaymentEvent,
    ) -> PaymentFuture<'_, CheckoutIntent<()>> {
        self.calls.lock().expect("calls").claim += 1;
        let result = self
            .pending
            .lock()
            .expect("pending")
            .take()
            .filter(|intent| intent.intent_id == *verification.intent_id())
            .ok_or_else(|| {
                PaymentError::new(PaymentErrorCode::IntentUnavailable, "intent.not_pending")
            });
        Box::pin(async move { result })
    }
}

struct MemoryAudit {
    records: Mutex<Vec<PaymentAuthorizationAudit>>,
    calls: Arc<Mutex<Calls>>,
}

impl PaymentAuditPort for MemoryAudit {
    fn record_authorization(&self, audit: PaymentAuthorizationAudit) -> PaymentFuture<'_, ()> {
        self.calls.lock().expect("calls").audit += 1;
        self.records.lock().expect("audit records").push(audit);
        Box::pin(async { Ok(()) })
    }
}

struct MemoryAdmissions {
    commands: Mutex<Vec<ExternalCommand<()>>>,
    calls: Arc<Mutex<Calls>>,
}

impl CommandAdmissionPort<()> for MemoryAdmissions {
    fn submit(&self, command: ExternalCommand<()>) -> CommandAdmissionFuture<'_, CommandResult> {
        self.calls.lock().expect("calls").admit += 1;
        let result = admitted_result(&command);
        self.commands.lock().expect("commands").push(command);
        Box::pin(async move { Ok(result) })
    }
}

struct Harness {
    orchestrator: PaymentOrchestrator<()>,
    calls: Arc<Mutex<Calls>>,
    audit: Arc<MemoryAudit>,
    admissions: Arc<MemoryAdmissions>,
}

fn harness() -> Harness {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let provider = Arc::new(FakeProvider {
        provider_id: payment_text(PaymentProviderId::new, "test-provider"),
        verified_intent: payment_text(CheckoutIntentId::new, "checkout-42"),
        calls: Arc::clone(&calls),
    });
    let intents = Arc::new(MemoryIntents {
        pending: Mutex::new(None),
        calls: Arc::clone(&calls),
    });
    let audit = Arc::new(MemoryAudit {
        records: Mutex::new(Vec::new()),
        calls: Arc::clone(&calls),
    });
    let admissions = Arc::new(MemoryAdmissions {
        commands: Mutex::new(Vec::new()),
        calls: Arc::clone(&calls),
    });
    let orchestrator = PaymentOrchestrator::new(
        provider,
        intents,
        admissions.clone(),
        audit.clone(),
        text(PrincipalId::new, "payment-orchestrator"),
    );
    Harness {
        orchestrator,
        calls,
        audit,
        admissions,
    }
}

#[test]
fn browser_and_target_success_assertions_never_reach_verification_or_admission() {
    for input in [
        PaymentAuthorizationInput::BrowserAssertion {
            intent_id: payment_text(CheckoutIntentId::new, "checkout-42"),
            succeeded: true,
        },
        PaymentAuthorizationInput::TargetAssertion {
            intent_id: payment_text(CheckoutIntentId::new, "checkout-42"),
            succeeded: true,
        },
    ] {
        let harness = harness();
        let error = block_on(harness.orchestrator.authorize(input))
            .expect_err("untrusted assertion must fail");
        assert_eq!(error.code(), PaymentErrorCode::UnverifiedSource);
        let calls = harness.calls.lock().expect("calls");
        assert_eq!(calls.verify, 0);
        assert_eq!(calls.claim, 0);
        assert_eq!(calls.audit, 0);
        assert_eq!(calls.admit, 0);
    }
}

#[test]
fn provider_verification_is_audited_then_uses_common_command_admission() {
    let harness = harness();
    let presentation = block_on(harness.orchestrator.begin_checkout(checkout_request()))
        .expect("checkout created");
    assert_eq!(presentation.expires_at, timestamp(8));
    assert_eq!(
        format!("{:?}", presentation.client_data),
        "SensitivePaymentData([REDACTED])"
    );

    let result = block_on(harness.orchestrator.authorize(
        PaymentAuthorizationInput::ProviderEvent(PaymentProviderEvent {
            provider_data: SensitivePaymentData::new("signed-provider-event"),
        }),
    ))
    .expect("verified payment admitted");
    assert!(matches!(result.lifecycle, CommandLifecycle::Admitted));

    let calls = harness.calls.lock().expect("calls");
    assert_eq!(calls.create, 1);
    assert_eq!(calls.verify, 1);
    assert_eq!(calls.claim, 1);
    assert_eq!(calls.audit, 1);
    assert_eq!(calls.admit, 1);
    drop(calls);

    let audits = harness.audit.records.lock().expect("audit records");
    assert_eq!(audits[0].provider_id.as_str(), "test-provider");
    assert_eq!(audits[0].intent_id.as_str(), "checkout-42");
    assert_eq!(audits[0].request_id.as_str(), "command-42");
    assert!(!format!("{:?}", audits[0]).contains("provider-evidence-42"));
    drop(audits);

    let commands = harness.admissions.commands.lock().expect("commands");
    assert_eq!(commands[0].request.request_id.as_str(), "command-42");
    assert!(matches!(
        commands[0].origin,
        AuthenticatedCommandOrigin::Bridge { .. }
    ));
}

#[test]
fn payment_details_are_redacted_and_absent_from_default_contract_schemas() {
    let secret = "card-number=not-for-default-surfaces";
    let sensitive = SensitivePaymentData::new(secret);
    assert!(!format!("{sensitive:?}").contains(secret));

    for schema in [
        include_str!("../../contracts/schemas/v1.0/event-envelope.schema.json"),
        include_str!("../../contracts/schemas/v1.0/export-record.schema.json"),
        include_str!("../../contracts/schemas/v1.0/trace-record.schema.json"),
    ] {
        assert!(!schema.contains("payment_details"));
        assert!(!schema.contains("payment_token"));
        assert!(!schema.contains(secret));
    }
}

fn checkout_request() -> CheckoutRequest<()> {
    CheckoutRequest {
        intent: CheckoutIntent {
            intent_id: payment_text(CheckoutIntentId::new, "checkout-42"),
            command: CommandRequest {
                request_id: text(RequestId::new, "command-42"),
                correlation_id: None,
                resource: resource(),
                operation: CommandOperation::Start {
                    authorization_reference: None,
                },
                expires_at: timestamp(9),
            },
        },
        provider_data: SensitivePaymentData::new("card-number=not-for-default-surfaces"),
    }
}

fn admitted_result(command: &ExternalCommand<()>) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: command.request.correlation_id.clone(),
        resource: command.request.resource.clone(),
        return_route: CommandReturnRoute {
            request_id: command.request.request_id.clone(),
            origin: command.origin.clone(),
        },
        lifecycle: CommandLifecycle::Admitted,
        recorded_at: timestamp(3),
        observed_effects: Vec::new(),
    }
}

fn resource() -> ResourceRef {
    ResourceRef {
        bridge_id: text(BridgeId::new, "bridge-1"),
        station_id: text(StationId::new, "station-1"),
        resource: None,
        native_protocol_reference: None,
    }
}

fn timestamp(minute: u8) -> UtcTimestamp {
    let date = Date::from_calendar_date(2026, Month::September, 1).expect("date");
    let time = Time::from_hms(12, minute, 0).expect("time");
    UtcTimestamp::new(PrimitiveDateTime::new(date, time).assume_offset(UtcOffset::UTC))
}

fn text<T, E>(constructor: impl FnOnce(String) -> Result<T, E>, value: &str) -> T
where
    E: std::fmt::Debug,
{
    constructor(value.to_owned()).expect("valid test identity")
}

fn payment_text<T>(constructor: impl FnOnce(String) -> Result<T, PaymentError>, value: &str) -> T {
    constructor(value.to_owned()).expect("valid payment identity")
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}
