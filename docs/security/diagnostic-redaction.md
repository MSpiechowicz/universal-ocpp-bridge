# Diagnostic redaction boundary

The service converts raw observations into `SanitizedDiagnostic` exactly once, before a record can
enter a log, trace ring, live broadcast, diagnostic export, or API error. That value contains one
shared inert JSON serialization. Downstream sinks must copy or stream those bytes; they must not
retain the source observation, re-read raw values, apply their own masking rules, or render payload
content as HTML.

The input surface is deliberately typed:

- explicitly safe canonical identifiers, operation names, sizes, and configured endpoint labels
  may be disclosed;
- authorization tokens, credentials, endpoint secrets, and payment-sensitive values become a
  stable `[REDACTED]` marker;
- unknown vendor payloads remain opaque and become `[OMITTED: unknown_vendor_payload]` without
  inspecting field names or schema guesses.

Every inert record contains safe audit fields naming the classes disclosed, redacted, and omitted.
The audit trail contains classifications and counts, never source values. An omitted vendor payload
also sets the contract's truncation indicator so inspection remains honest.

Safe endpoint labels are configured display identities, not sanitized URLs. Raw addresses,
userinfo, queries, fragments, and credential-bearing endpoint configuration never enter the safe
label type.

Runtime identity is also a security boundary. Production rejects simulator and mock-checkout
controls. Payment-dependent application logic accepts only provider-verified evidence; a browser
`payment succeeded` assertion is untrusted in every environment.

OCPP 1.6 configuration values are classified before durable command results or diagnostics
are written. Only a small allowlist of numeric Core keys may expose validated decimal
values; unknown/vendor, credential-like or malformed values are omitted with a redaction
flag. Privileged ChangeConfiguration accepts an opaque, station/key-bound protected
reference rather than an inline secret. The SQLite command codec refuses inline write
values even if a caller bypasses normal admission. Queued writes contain only the reference:
the trusted provider rechecks revocation, scope and expiry at the socket-send boundary.
The encoded frame transiently holds the value; a revocation after sending starts cannot
cancel an in-flight WebSocket send. Native write acknowledgements and later observations
are distinct evidence, not claims of physical state change.

OCPP 2.0.1 protected `SetVariables` and `SetNetworkProfile` follow the same privacy
boundary. Public commands and durable queues carry references only; the owner-only
startup provider holds the actual values and full native network profiles. Reusable
`cfg201:` capabilities are sensitive too: they may be retained in the protected
command envelope, but are not diagnostic safe fields, result metadata or export
evidence. Native `connectionData`, CSMS URLs, APN/VPN usernames, passwords, keys and
SIM PINs are sensitive even when they look like ordinary configuration. The bridge
never fetches a profile URL, and a native URL is not a safe endpoint display label.

Typed `configuration_201` evidence contains only component/variable/attribute identity,
slot and native statuses, with a separate network staging flag. It omits values,
profiles, capabilities and native `statusInfo`/`customData`; later observed effects
remain independent. Invalid provisioning and admission errors do not reproduce
rejected private material. An independent actual-daemon smoke exercised enabled,
redacted capture and verified synthetic private values/profiles/capabilities were
absent from public results and capture; private values were also absent from
SQLite/WAL and logs.

Bridge-owned provisioning, decoded secret and transient wire buffers are wiped at
their ownership boundaries. This is not a claim that allocator spare memory,
third-party parser/transport buffers, operating-system copies or bytes already
transmitted are erased. Revocation, tightened limits and socket-generation detachment
fence writes before the first send poll; after asynchronous transmission begins,
the bridge cannot unsend bytes. Stop, rotate the private file with fresh independent
capabilities, and restart to replace daemon provisioning; there is no hot reload or
public secret/revocation endpoint.

OCPP 1.6 station authorization-list updates also use protected startup content and
reference-only durable envelopes. `list16:` capabilities, raw `idTag` and
`parentIdTag` values are never safe diagnostic fields, public command parameters,
typed `local_authorization_16` evidence, snapshots, SSE, capture or export payloads.
The typed evidence carries only the action, requested version/update type, query
version or exact native status. Native error text and malformed/unpaired reply
content are not projected. The protected command envelope may retain a capability
for durable deduplication; SQLite/WAL never retain raw list or parent identities.

The independent actual-daemon smoke exercised Full/Differential/delete, all native
Send statuses, signed version queries, delayed/lost/malformed/unpaired responses,
enabled capture and restart/deduplication. Synthetic raw identities, parent tags and
capabilities were absent from public history, snapshots, retained SSE and capture;
raw identities and parents were also absent from SQLite/WAL and service logs.
Bridge-owned provisioning and transient native buffers are wiped at their ownership
boundaries, with the same allocator/third-party/operating-system limitations above.
The simulator's separate owner-only recovery file necessarily retains native tags
for genuine offline authorization and replay; its public progress and traces do not.
