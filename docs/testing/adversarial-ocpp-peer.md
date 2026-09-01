# Adversarial OCPP WebSocket peer

`uob-hostile-websocket-peer` is a test-only, low-level Rust WebSocket driver for inputs that a
normal OCPP client would reject before transmission. It is a separate workspace crate and must not
depend on `ocpp-client`, `rust-ocpp`, bridge contracts, adapters, or service code. The boundary check
enforces that dependency rule.

The peer negotiates `ocpp1.6` or `ocpp2.0.1`, but otherwise treats application messages as opaque
text or binary data. A versioned TOML scenario can therefore send malformed JSON, invalid OCPP array
shapes, schema-invalid payloads, duplicate IDs, unmatched replies, and deliberately reordered
exchanges. `repeated_text` creates exact just-below/above-bound payloads without checking a large
fixture into the repository. Each step has a real wall-clock timeout and explicit text, binary,
close-code, transport-error, or continued-service expectation.

See
[`tests/hostile-websocket-peer/scenarios/protocol-errors.toml`](../../tests/hostile-websocket-peer/scenarios/protocol-errors.toml)
for the scenario format. Text response expectations can use an RFC 6901 JSON pointer and exact value,
so schema failures can assert a useful field path such as `/4/path`.

Observations are held in a fixed-capacity ring. They record direction, frame kind, byte count, and a
SHA-256 digest; raw payloads are never copied into the observation log. The peer's own outbound and
inbound safety bounds are independent of the bridge limit under test, allowing a scenario to send
both sides of a bridge boundary without allocating unbounded data.
