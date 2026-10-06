# Bounded multipart report collection

`uob_protocol_adapter::multipart::collect_report` provides reusable collection for native
device-model, monitoring, and other multipart report owners. It is an awaited future with no
spawned worker, background timer, detached task, or internal queue. The OCPP 2.0.1 read-only
[device-model workflow](ocpp201-remote-control.md#opt-in-read-only-device-model-queries) (#109)
uses this policy with connection-owned supervision. Monitoring (#126) remains a separate
planned workflow; a reusable collector is not a blanket feature-coverage claim.

## Correlation and sequence

The requesting workflow assigns a `ReportKey` containing the authenticated station, connection
generation, protocol edition, report action, native request ID, and command correlation. These
fields must all match every fragment. Connection and station identities come from the socket
owner, never charger-supplied claims. Keys are limited to 128 bytes per textual field; native
request IDs use the full signed 32-bit representation, unchanged on the wire. Sequence numbers
remain nonnegative.

Register exactly one collector per connection/report namespace/request ID before sending its
request. GetBaseReport and GetReport share `NotifyReport`: native request ID reuse across either
action is rejected before wire. GetChargingProfiles owns the separate `ReportChargingProfiles`
namespace, so the same signed ID may be open once in each. Both namespaces share the four route
slots, and a budgeted, fixed-capacity used-ID set retires at most 4,096 IDs per connection until
teardown, including after a collector finishes. `ReportChargingProfiles` has no native sequence
number; the socket owner assigns arrival order, which is the order of one connection's frames. A late fragment carrying a
reused native ID cannot distinguish two commands. The source routes only the registered request
and unregisters on drop. It must use bounded ingress from the existing socket-owning call
lifecycle, validate the full frame before decoding, and retain ingress admission through handoff.
Source futures must be cancellation-safe and must not block the runtime thread. This adapter
contract does not authorize opening a second socket reader or an unbounded forwarding queue.

Native decoders map sequence and continuation into `ReportFragment`. The collector preserves the
caller-supplied validated item bytes; device-model routing supplies sanitized typed items, not
opaque native values. Sequence starts at zero and must advance by exactly
one. A lower sequence is an explicit duplicate/conflict error, even if its content is identical;
a higher sequence is a missing/out-of-order error. Neither silently appends nor restarts a report.
The first fragment with `more=false` completes collection, including an empty final fragment.
This is not by itself durable device-model completion: that workflow also requires a valid native
Accepted acknowledgement. Later fragments belong to the caller's late/unsolicited-report policy
and cannot reopen this collector or rewrite finalized evidence.

## Bounds and lifecycle

Defaults are 1 MiB of retained item bytes, 4,096 items, 256 fragments, and a 30-second absolute
deadline. Configurable hard ceilings are 8 MiB, 65,536 items, 4,096 fragments, and 300 seconds;
zero or larger values fail before polling ingress. The caller passes an explicit actual
dispatch-start `Instant`; the deadline is that instant plus `ReportLimits.timeout`, not collection
poll time, admission time, native ACK or first-fragment arrival. Per-fragment item bytes must also
fit the process OCPP message cap; this is additional to validation of the complete ingress frame.
The composed device-model workflow uses four shared assembly slots, a 16 MiB queue byte budget
and a 256 KiB complete-frame cap without consuming the protected critical reserve.

Every collection reserves one shared `MultipartAssembly` slot before allocating. It also reserves
the fixed item-descriptor array and bounded result/key metadata. Each fragment's cumulative byte
and item counts are checked before its contents are copied into tightly sized buffers, and the
shared reservation grows first. Spare capacity in incoming vectors is not retained. Native
decoding and transient input remain the ingress owner's responsibility. Allocation metadata and
allocator bookkeeping are not claims of exact RSS accounting.

Report memory competes with other noncritical work under the existing aggregate byte cap and
cannot consume the capacity reserved for charger requests and critical reports. A capacity error
fails immediately rather than waiting for charging to release memory. Even continuously ready
fragments yield between acceptance steps so a flood cannot monopolize the runtime thread.

Timeout is absolute and is not renewed by fragments. Cancellation wins over a simultaneously
ready fragment; expired reports cannot complete just because input is ready. Disconnect, decoder
error, cancellation, timeout, correlation/sequence errors, or any limit breach returns a
`PartialReport` with the expected key, accepted fragment/item/byte counts, and an explicit reason.
It contains no partial payload and cannot be confused with a completed report. All partial
buffers and their shared reservation are dropped before return. Dropping the collection future
also drops its source, buffer, timer, and reservation without background cleanup work.

Only `CollectedReport` exposes ordered items. It retains its byte and assembly-slot reservation
until the consumer drops it; finishing collection cannot hide retained memory from admission.
Consumers borrow items rather than extracting unaccounted buffers. Any deliberate downstream
copy requires its own budget reservation. Native report content is not diagnostic-safe:
device-model ingress sanitizes it before retained serialization, persistence, capture or export.
Bounded reservations precede escaped serialization expansion and copying; device-model output has
a separate 1 MiB escaped-JSON bound and an explicit `output_limit` incomplete outcome.

## Verification

`cargo test --locked -p uob-protocol-adapter --test multipart` exercises ordering and every
correlation field, exact/excess byte/item/fragment limits, empty fragments, duplicate/conflicting
and broken sequences, shared capacity and protected charging capacity, and completed-result
retention. Paused-clock tests verify missing fragments, absolute deadlines, cancellation, source
drop, and cleanup. A continuously ready source test verifies unrelated runtime progress.

The full workspace verifier includes this suite. These are collection behavior tests with
controlled sources, not proof of monitoring or complete device-model coverage. Native read-only
query/report integration is documented separately; complete-release qualification remains gated.
