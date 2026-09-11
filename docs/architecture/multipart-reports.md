# Bounded multipart report collection

`uob_protocol_adapter::multipart::collect_report` provides reusable collection for native
device-model, monitoring, and other multipart report owners. It is an awaited future with no
spawned worker, background timer, detached task, or internal queue. This implements collection
policy, not the device-model or monitoring business workflows tracked separately by #109 and #126.
It adds no unconditional OCPP success handler or feature-coverage claim.

## Correlation and sequence

The requesting workflow assigns a `ReportKey` containing the authenticated station, connection
generation, protocol edition, report action, native request ID, and command correlation. These
fields must all match every fragment. Connection and station identities come from the socket
owner, never charger-supplied claims. Keys are limited to 128 bytes per textual field and native
request IDs to nonnegative signed 32-bit values.

Register exactly one collector per connection/action/request ID before sending its request. Do
not reuse a request ID for that report kind within the connection: a late fragment carrying a
reused native ID cannot distinguish two commands. The source routes only the registered request
and unregisters on drop. It must use bounded ingress from the existing socket-owning call
lifecycle, validate the full frame before decoding, and retain ingress admission through handoff.
Source futures must be cancellation-safe and must not block the runtime thread. This adapter
contract does not authorize opening a second socket reader or an unbounded forwarding queue.

Native decoders map the sequence number and continuation flag into `ReportFragment`, preserving
each validated native item's encoded bytes. Sequence starts at zero and must advance by exactly
one. A lower sequence is an explicit duplicate/conflict error, even if its content is identical;
a higher sequence is a missing/out-of-order error. Neither silently appends nor restarts a report.
The first fragment with `more=false` completes it, including an empty final fragment. Later
fragments belong to the caller's late/unsolicited-report policy and cannot reopen this collector.

## Bounds and lifecycle

Defaults are 1 MiB of retained item bytes, 4,096 items, 256 fragments, and a 30-second absolute
deadline. Configurable hard ceilings are 8 MiB, 65,536 items, 4,096 fragments, and 300 seconds;
zero or larger values fail before polling ingress. Per-fragment item bytes must also fit the
process OCPP message cap; this is additional to validation of the complete ingress frame.

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
copy requires its own budget reservation. Native report content is not diagnostic-safe: it must
cross the central redaction boundary before trace/export and must be rendered as inert data.

## Verification

`cargo test --locked -p uob-protocol-adapter --test multipart` exercises ordering and every
correlation field, exact/excess byte/item/fragment limits, empty fragments, duplicate/conflicting
and broken sequences, shared capacity and protected charging capacity, and completed-result
retention. Paused-clock tests verify missing fragments, absolute deadlines, cancellation, source
drop, and cleanup. A continuously ready source test verifies unrelated runtime progress.

The full workspace verifier includes this suite. These are collection behavior tests with
controlled sources, not a claim that the later native report business workflows are implemented.
