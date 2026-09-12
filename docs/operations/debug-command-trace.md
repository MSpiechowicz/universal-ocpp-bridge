# Command evidence in Debug

Open any retained timeline row to see the command evidence panel. Authorization, coordinator
admission, dispatch, charger response, observed effect, API exposure, target reports and duplicate
handling have independent rows. There is no overall success indicator. An HTTP-exposed result does
not prove dispatch, a peer acknowledgement does not prove charger acceptance, and charger acceptance
does not prove a physical charging effect. Unknown stage/evidence combinations stay unclassified.

The current coordinator emits `command.request_id` on command stages. The panel joins only exact
request identities within the same process and station. Traces sharing only correlation are separate
navigation links, including target reports, state observations and other requests. They cannot fill
this request's missing stages. Older records without request metadata show only their own evidence;
missing identifiers are never guessed from neighboring rows. Summary and links cover the retained
window even when the timeline has display filters, but obey the pause ceiling and eviction.

Typed, centrally redacted fields additionally expose:

- `command.origin`: adapter-authenticated principal and originating target routing identity; no token.
- `reason_code`: closed access-policy and command rejection categories, including `StationDisconnected`.
- `command.event_id`: the durable event explicitly linked after observed-effect persistence.

Raw command payloads and raw error strings are not copied. Origin/request/event identifiers are
bounded at the producer. Imported-looking strings in browser fixtures render as inert text. The
panel does not submit commands, poll results, enable capture, or replay anything. It uses the
existing authenticated capture stream and opens linked rows locally.

`duration_micros` is displayed as local span elapsed, not per-stage latency. Different spans can
have different timing origins even with matching request/correlation identities. Queue-only wait
is explicitly unavailable because current instrumentation does not measure it independently.
Uncertain transmission is shown with the existing no-automatic-replay policy; duplicate detection
does not assert that a new dispatch happened. Missing timeout detail remains unavailable rather
than being inferred from an absent response.

At most four observations per stage and 64 links per list are mounted, with explicit omitted
counts. There is no second command-history buffer: links disappear when the shared trace ring is
cleared or evicts them. Its existing 2,000-row / 4 MiB encoded-trace limit bounds input; compact
scalar indexes add bounded per-row metadata and are not a total-heap measurement. Gaps, capture
scope and truncation remain visible; no view claims to reconstruct a complete command history.

Verification: `npm --prefix frontend run check`, `npm --prefix frontend run test:browser`, and
`./scripts/verify-workspace.sh`. Browser acceptance uses the real capture API with a controlled
trace stream to prove API-only, acknowledged-without-effect, timeout, disconnected, denied and
explicit-effect displays. Real OCPP 1.6J and 2.0.1 socket tests independently verify producer request
metadata and accepted versus missing-response behavior. These are distinct layers of evidence;
the controlled browser stream is not an end-to-end charging demonstration.
