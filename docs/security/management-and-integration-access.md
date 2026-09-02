# Management and integration access policy

The production service binds its management API to `127.0.0.1:8080` by default. The reserved
direct EMS/SCADA HTTP listener defaults to `127.0.0.1:9080`. Neither default accepts traffic from
an external interface, and the current low-level management server refuses a non-loopback socket
so callers cannot bypass startup policy validation.

Remote management or direct EMS/SCADA access is an explicit deployment choice. Before an adapter
binds a non-loopback listener, offline validation requires all of the following:

- remote access is deliberately enabled;
- a TLS server certificate and private key are referenced through the protected credential
  boundary;
- at least one authenticated principal has explicit operation permissions and canonical bridge,
  station, EVSE, or connector scopes.

Credential files and resolved secret values are adapter-owned and are not serialized into safe
configuration, errors, or diagnostics. Duplicate credential references and duplicate management
principals fail validation. A credential without a permission or resource scope cannot be
constructed. Loopback is not an authorization boundary for command handlers: when command
endpoints are enabled, they still attach an authenticated origin and use the same scoped command
port.

## Shared authorization boundary

`AccessPolicy` maps an authenticated management or target origin to one or more explicit grants:

- `read` permits canonical snapshots, points, capabilities, command status, and retained events;
- `control` permits ordinary typed charging commands;
- `privileged_control` permits pinned-schema OCPP management operations;
- resource scopes grant one bridge, one station and its descendants, or one exact canonical
  resource.

The adapter authenticates the connection or request and attaches the resulting trusted origin;
origin fields never come from the submitted command body. `ScopedCommandAdmissionPort` rejects an
origin mismatch, a missing operation permission, or an out-of-scope resource before the request
reaches durable admission. The inner application command port still reapplies capability, safety,
expiry, idempotency, and application authorization rules. Transport authentication therefore
cannot turn protocol acceptance into authorization or bypass local charging policy.

The target-session host applies the same command guard around target adapters. Direct HTTP clients
may have different principals and grants in one target policy; a read-only integration credential
cannot submit a command even when its TLS or bearer authentication succeeded.

## MQTT ACL equivalence

Broker ACLs must express the same separation without replacing application checks:

| Credential class | Broker access | Application grant |
|---|---|---|
| Observer | Subscribe to allowed state, event, result, and availability topics | `read` on the same canonical resources |
| Command publisher | Publish only non-retained commands for allowed resources; subscribe to correlated results | `control` on the same canonical resources |
| Privileged publisher | Publish only explicitly supported privileged-operation topics or payloads | `privileged_control` on the same canonical resources |

The configured environment, bridge identity, target instance, and resource mapping are trusted
adapter context, not values authorized by an incoming MQTT topic or JSON field. Retained, expired,
unscoped, or unauthorized commands fail before durable admission. Broker authentication and ACLs
are defense in depth; every accepted MQTT command still traverses the shared scoped port and the
application command authorization path.
