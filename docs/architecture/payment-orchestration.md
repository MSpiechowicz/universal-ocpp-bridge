# Payment orchestration boundary

Payment verification is an application/provider concern independent of the selected bridge target.
The payment provider receives checkout and callback details through `SensitivePaymentData`; that
type is not serializable or cloneable and always renders as `[REDACTED]` in debug output. Default
events, target deliveries, external exports, diagnostics, and target configuration schemas have no
payment-detail variant.

The application binds a `CheckoutIntentId` to a typed canonical `CommandRequest` before checkout.
A configured `PaymentProvider` may return `VerifiedPaymentEvent` only after validating its raw
callback. The `PaymentIntentStore` atomically claims the pending binding once, preventing callback
replay from repeatedly starting charging. Browser/WebView and selected-target success assertions
are explicitly rejected without invoking the provider, intent store, audit sink, or command path.

Before a verified command is submitted, `PaymentAuditPort` records the safe provider identity,
checkout identity, opaque evidence reference, request identity, and verification time. It never
receives payment payloads. The command then enters the same `CommandAdmissionPort` used by other
authenticated ingress. That port reapplies authorization, capability, safety, expiry,
idempotency, and durable-admission policy; provider verification is evidence, not permission to
bypass charging policy.

Provider SDKs and credentials belong in outward provider adapters. A station WebView, management
API, or future EMS payment application may expose a scoped checkout surface without selecting or
starting MQTT. The composition root supplies the payment provider independently of
`BridgeTarget`, including when the direct HTTP target or no bridge target is selected.
