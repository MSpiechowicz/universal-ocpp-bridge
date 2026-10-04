# Simulator OCPP client adapter

`uob-sim` pins `ocpp-client` 0.5.0 as its normal WebSocket transport and keeps it behind the
simulator-owned `ProtocolClient` interface. The simulator package does not depend on service,
domain, application, persistence, or protocol-adapter packages, and the production service does
not link or launch the simulator.

The reviewed release provides OCPP 1.6J and 2.0.1 WebSocket negotiation, typed outbound calls,
typed inbound handlers, request deadlines, reconnect backoff, keepalive, and explicit disconnect.
The adapter selects only the two planned versions, supplies its own bounded command queue and
bounded trace ring, disables unsolicited keepalive traffic for deterministic scenarios, and
turns upstream failures into explicit simulator errors. It never returns a fake successful OCPP
response.

## Proven limitations

- The dependency is pre-1.0 and its 0.5.0 public API requires Rust 1.87. The workspace deliberately
  sets a higher Rust 1.98 floor, and the fallback verification image moves with that workspace pin.
- WebSocket/WSS is the only production-ready transport in this release. The upstream embedded
  transport is experimental and is not enabled by `uob-sim`.
- The client owns transport and OCPP request routing, not charger state or scenario behavior.
  The project-owned bounded OCPP 1.6 list/cache/offline model implements those behaviors.
- Reconnect preserves registered handlers but cannot make an in-flight call certain after a
  disconnect. The adapter reports the timeout/transport result and scenarios must reconcile it.
  Ordinary OCPP 1.6 automatic reconnect retains prior Accepted registration for
  charging calls, but never promotes Pending/Rejected or implicitly sends Boot.
  Boot-trigger eligibility uses acceptance on the current socket, not registration
  retained from an earlier socket. Persistent recovery and native Reset continue
  to fence charging until a real fresh Boot is Accepted.
  Late responses cannot overwrite current Boot/registration state or authorize
  replay on a replacement socket. Recovery calls and explicit persistent-model
  replay are generation-bound through every native transport send poll.
  If reconnect Boot is Pending/Rejected, an actual current-socket Accepted
  triggered Boot also requests automatic durable replay. Accepting TriggerMessage
  alone does not authorize replay; a lost native Start reply remains uncertain and
  is not retransmitted after reopening state and establishing a new native socket.
- Project-side queues and traces are bounded. The dependency's internal bookkeeping is not a
  public capacity-configurable queue; its reviewed timeout/disconnect cleanup tests are relied on,
  while project socket tests verify calls terminate within configured bounds.

Focused real-socket tests cover both subprotocol handshakes, outbound Heartbeat, inbound Reset,
timeouts, reconnect notification, deliberate shutdown, and bounded project buffers. Cargo
metadata and repository boundary checks demonstrate that simulator and service remain distinct
packages and dependency paths.

## Compose target demonstration

The [isolated Compose profiles](../testing/compose-profiles.md) run `uob-sim` as its own
non-root image, separate from the bridge daemon and the target-side verification client.
Both 1.6J and 2.0.1 charger sockets stay in the per-run Compose network namespace,
with per-station credentials file-mounted rather than placed on a command line.
The target client sends real remote start/stop commands; the adapter delivers and
acknowledges each incoming OCPP request, while the scenario independently reports
native transaction and meter evidence. An accepted remote command by itself is
not a simulated physical transaction or a production charging guarantee.

## Persistent native OCPP 1.6 authorization

Configure `[stations.local_authorization]` with a separate `private_state_file` to
enable genuine station-owned list/cache/offline behavior. `list_supported` and
`cache_supported` default to true only for this explicitly configured persistent
model. Without it, native Get returns `-1`, Send returns `NotSupported`, and Clear
returns `Rejected`; an in-memory runtime model cannot pretend to persist Accepted.

The owner-only state file binds format version, station and protocol. A canonical
mode `0700` owner directory, mode `0600` owned file, inode/alias checks and an
exclusive single-writer lock protect it. Atomic replacement precedes acknowledgement.
Pre-replacement failure preserves old state; uncertain post-replacement directory
sync makes state unavailable until explicit successful disk recovery. Corrupt,
foreign/version-mismatched or insecure files fail instead of silently resetting.
Limits are 256 list entries, 256 cache entries, 128 offline records and 2 MiB.

Simulator-owned native wire DTOs preserve the native 20-Unicode-character tag and
parent limit rather than the dependency's 20-byte `CiString20` restriction. Full
Unicode casefold matching affects local identity lookup only: original spelling,
native statuses, expiry, parents, signed list versions and `i64` transaction/meter
values remain intact. Identities in an enabled list take priority, are not newly
inserted into the cache, and can emit connector-zero `LocalListConflict` without
blocking a known native result on that separate notification acknowledgement.
A retained disabled list does not suppress the enabled cache. Both maps survive
support toggles and recovery; re-enabling the list restores its priority.

Actual CSMS-offline actions close the socket. Reconnection performs native Boot and
bounded replay using returned transaction IDs and original timestamps. Native Reset
is acknowledged before actual close, disk recovery, new socket and Boot; it is not
equated with killing/restarting the CLI. In-flight transaction uncertainty is durable
and not automatically replayed. Request/replay tasks and traces are bounded, with
generation-aware socket liveness. See [scenario actions](scenario-runner.md#native-ocpp-16-local-list-cache-and-real-offline-recovery).

Independent software-peer smokes exercised real native Reset/recovery, post-reboot
offline authorization/replay, delayed durable ACK and lost ACK/explicit reconnection/
fresh Full. These are software-boundary results, not charger hardware qualification,
OCA certification, exactly-once execution or third-party memory-erasure guarantees.
