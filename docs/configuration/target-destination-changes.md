# Target destination changes

Target instance identity and configuration revision form the immutable destination of every
critical target delivery. The service stamps both values when it admits work and must compare them
again before dispatch. A payload owned by another instance or revision is never rewritten for the
currently selected target.

Configuration clients use the offline target-change preview before activation. Preview validates
the registry, schema, transport policy, and driver settings, but it does not construct an adapter,
open a socket, replace the running session, or apply a configuration. When the validated
destination differs from the running destination, preview reports `restart_required`; target
changes only become active during a service restart.

The host supplies a bounded summary of pending critical deliveries grouped by exact destination.
A changed destination is rejected while any old owner has pending work. The only alternatives to
draining are an already-authorized archive or discard operation with a non-empty durable audit
event for every exact old owner. Extra, duplicate, missing, or revision-mismatched proofs fail
closed. The caller must apply those audited dispositions before consuming the preview as restart
configuration.

Archive and discard never transfer ownership. Even after an authorized disposition, the runtime
delivery guard rejects recovered or queued payloads whose instance or revision differs from the
validated startup selection. Command results independently retain the authenticated origin and
target instance captured at admission, so a later configuration change does not alter their return
route.
