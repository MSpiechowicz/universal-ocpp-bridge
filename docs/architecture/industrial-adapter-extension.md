# Industrial adapter extension boundary

Issue #28 reserves a reviewable path for future industrial protocols without shipping or implying
support for them in the first release. `ems-scada.opcua` is a recognized catalog kind, but it has no
factory and selection fails with `UnavailableKind`. The workspace links no OPC UA SDK. The supported
first-release EMS/SCADA paths remain the direct HTTP/SSE adapter and the `ems-scada` preset of the
MQTT adapter when those implementations are present.

## Addition checklist

A future industrial driver is complete only when one change set provides all of the following:

- a separate adapter package containing transport, protocol, security, and mapping code;
- a stable target kind and adapter-owned configuration schema with credential references rather
  than credential values;
- factory registration in the service composition root, replacing the matching unavailable catalog
  declaration only when the implementation and its tests are included;
- the reusable target conformance suite plus protocol-specific security, reconnect, subscription,
  interoperability, mapping-failure, and resource-bound tests; and
- documentation of supported server/client role, profiles, operations, data types, limits, and
  compatibility evidence without a generic vendor-compatibility claim.

No charging workflow, domain type, or application service gains a driver-kind switch. An adapter
uses `BridgeTarget`, `TargetContext`, `TargetQueryPort`, and `TargetCommandPort`; the composition root
is the only place that knows its concrete factory. HTTP, MQTT, and future adapters therefore share
canonical behavior without importing or calling one another.

## Canonical-to-industrial mapping checklist

The adapter owns its address-space or vendor-model identifiers. Its mapping specification must
trace every exposed item back to these existing canonical fields:

| Industrial concern | Canonical source and required preservation |
|---|---|
| Object hierarchy and stable identity | `DataPointDescriptor.resource` (`bridge_id`, `station_id`, optional charging resource, and native protocol reference), plus `point_id`; an industrial NodeId or register is mapping data, never canonical identity. |
| Variable meaning and type | `semantic_name`, `value_type`, `access`, and `constraints`; unsupported types or ranges fail explicitly. |
| Engineering quantity | Descriptor `unit` and `DataPointValue.value`; decimal values remain exact and are never routed through binary floating point. |
| Source semantics | `measurement.original_value`, `original_unit`, measurand, phase, context, location, and protocol reference remain available for a lossless or explicitly rejected mapping. |
| Status and time | `quality.level`, `quality.reason`, `freshness`, `source_time`, and `observed_at` map independently; bad, stale, unknown, and unavailable values never become zero or false. |
| Command identity and target | `ExternalCommand.request.request_id`, correlation ID, canonical resource, expiry, and authenticated `Target` origin; payload node, topic, or requester text never supplies authorization. |
| Command result | `CommandResult.return_route`, lifecycle, recorded time, and separately linked observed effects; protocol acceptance is not reported as physical effect. |

Mapping a value or operation is all-or-explicit-error: an adapter must not silently drop units,
precision, quality, timestamps, request identity, origin, or unsupported command semantics.

## Future OPC UA decisions

A server-style `EmsScadaOpcUaTarget` would map resource and point descriptors into an address space
and publish values with source/server timestamps and quality. Commands use explicit methods or
declared idempotent desired-state writes submitted through `TargetCommandPort`. Reading or
subscribing to a variable cannot create a command. Repeated observation, polling, subscription
delivery, or an unchanged desired-state write cannot repeat a start operation.

The implementation issue must choose and test, rather than pre-decide here:

- namespace URI/versioning and deterministic NodeId conventions;
- canonical quality and mapping-error conversion to OPC UA status codes;
- supported security policies, trust stores, certificate enrollment/rotation, and endpoint roles;
- subscription limits, queue/discard behavior, sampling intervals, and reconnect semantics;
- SDK ownership, pinning, advisories, feature set, cross-compilation, and bounded Pi resources; and
- independent client interoperability fixtures for every advertised type and operation.

A vendor-facing client mode is a separate registered kind with its own schema and compatibility
evidence. It is not a mode switch inside the canonical application workflow.

## Compatibility and connection limits

- An MQTT broker does not convert canonical MQTT messages into an arbitrary vendor HTTP API.
- The direct EMS/SCADA HTTP target does not imply compatibility with products that expose a
  different HTTP contract.
- An OPC UA or vendor adapter does not add implicit simultaneous control targets or failover.
- Target selection remains explicit; an unavailable or failed target never falls back to MQTT.
- Adding an adapter does not establish broad EMS/SCADA vendor compatibility. Only the documented,
  independently tested protocol profile and mappings are supported.
