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
