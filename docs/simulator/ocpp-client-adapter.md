# Simulator OCPP client adapter

`uob-sim` pins `ocpp-client` 0.4.0 as its normal WebSocket transport and keeps it behind the
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

- The dependency is pre-1.0 and its 0.4.0 public API requires Rust 1.87. The workspace deliberately
  sets a higher Rust 1.98 floor, and the fallback verification image moves with that workspace pin.
- WebSocket/WSS is the only production-ready transport in this release. The upstream embedded
  transport is experimental and is not enabled by `uob-sim`.
- The client owns transport and OCPP request routing, not charger state or scenario behavior.
  Those remain project code in later simulator backlog items.
- Reconnect preserves registered handlers but cannot make an in-flight call certain after a
  disconnect. The adapter reports the timeout/transport result and scenarios must reconcile it.
- Project-side queues and traces are bounded. The dependency's internal bookkeeping is not a
  public capacity-configurable queue; its reviewed timeout/disconnect cleanup tests are relied on,
  while project socket tests verify calls terminate within configured bounds.

Focused real-socket tests cover both subprotocol handshakes, outbound Heartbeat, inbound Reset,
timeouts, reconnect notification, deliberate shutdown, and bounded project buffers. Cargo
metadata and repository boundary checks demonstrate that simulator and service remain distinct
packages and dependency paths.
