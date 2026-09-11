# OCPP 2.0.1 charger authorization

`v201::authorize_call` and `complete_authorization` process `Authorize` through the existing
bounded incoming-call/responder lifecycle. The host supplies the authenticated station resource,
recovered `LocalAuthorizationService`, trusted clock, and selected `ChargingIdentityProvider`.
Use the same ordered station owner as registration; a response is sent through the incoming
call's responder only after completion. Provider resolution must finish within a configured
nonzero timeout of at most 30 seconds, within the host's call deadline.

The application-owned `PresentedChargingIdentity` preserves token type, original token,
additional identity/type pairs, PEM certificate evidence, and all OCSP hash/URL fields. Its Debug
representation redacts the entire input. These inputs are sensitive: providers must neither log
nor retain them. A supplied OCSP responder URL is untrusted data, not authorization for outbound
network access; a PKI provider must enforce its own trusted destination and certificate policy.
The protocol adapter performs no OCSP network access.

The offline `LocalChargingIdentityProvider` resolves ordinary token types to a SHA-256 reference
using a versioned namespace, exact token type, and uppercased token. OCPP 2.0.1 identifiers are
case insensitive. References cannot alias between token types or with the raw-token OCPP 1.6
provider. Provision the typed provider's reference in the durable local allowlist. It fails closed
for eMAID, NoAuthorization, additional identities, or certificate evidence; those require an
explicitly configured provider that understands their semantics. NoAuthorization is not an
implicit permission to charge. Composition roots must apply the existing test-only provider
policy to provider descriptors, including this additional typed provider interface.

Successful resolution only supplies an opaque reference. The service consults the latest durable
local policy **after** awaiting the provider, using current server time. Revocation and expiry
during a delayed lookup therefore take effect before replying. Restart/reconnect restores the
same persisted policy; there is no positive adapter cache. Provider failure or timeout returns
Invalid, never speculative acceptance. A failed certificate result, or missing certificate
verification when evidence was supplied, also prevents acceptance.

Local unknown, revoked, expired and resource-denied decisions map respectively to Unknown,
Blocked, Expired and NotAtThisLocation. Providers can additionally report ConcurrentTx, NoCredit,
NotAllowedTypeEVSE and NotAtThisTime. Certificate status is independent of token permission:
Accepted certificate evidence cannot override local revocation. Only an allowed local decision
can supply cacheExpiryDateTime. No transaction state, charging effect, or payment success is
inferred from an Authorize response.

The pinned OCA Edition 4/June 2026 archive and schema bundle hashes are recorded in the fixture
corpus provenance. Authorize request/response schemas are copied byte-for-byte from that bundle.
Independent request, certificate-evidence, accepted and blocked wire fixtures are schema checked.
The synthetic certificate fixture exercises evidence forwarding only; it is not a valid
certificate or a PKI validation claim. Certificate-chain lifecycle coverage remains separate.

Malformed fields, unknown token/hash enumerations, explicit nulls, empty tokens, oversized nested
values, empty arrays and more than four certificate hashes are rejected before provider work.
Additional identities are limited to 16 as a host resource policy. Authorization customData is
explicitly unsupported rather than silently discarded or interpreted as permission. The existing
socket payload and queue budgets bound wire admission. Error messages contain no identity data.

`ocpp201_authorization` tests cover durable allow/deny/expiry/scope/recovery, delayed expiry and
revocation, timeout, provider denials, evidence preservation, type isolation, redaction, invalid
inputs, and authenticated WebSocket replies across reconnect. Full certificate validation,
transaction admission, payment orchestration, and certification are separate features.
