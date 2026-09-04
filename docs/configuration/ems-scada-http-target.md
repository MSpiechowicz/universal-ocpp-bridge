# EMS/SCADA HTTP target

The `ems-scada.http` target is the direct EMS/SCADA integration listener. It serves a versioned
integration API under `/bridge/v1` for external industrial clients. It starts no broker, opens no
outbound client, and mounts no management, debug, capture, or simulator route.

The independent management API stays available in this mode on its own loopback listener. The two
surfaces reuse the same application query and command ports and the same canonical schemas; they
do not duplicate business handlers or storage.

## Configuration

`listen_addr` is the single listener endpoint. Do not add the generic `targets.transport` table to
an `ems-scada.http` entry: startup rejects two independently configurable endpoints, and this
listener owns its own exposure rule.

```toml
[bridge]
id = "site-01"
environment = "production"
target_id = "main"

[[targets]]
id = "main"
kind = "ems-scada.http"
enabled = true
revision = 1

[targets.settings]
listen_addr = "127.0.0.1:9080"
credentials_file = "/run/secrets/uob-ems-api.toml"
```

| Setting | Meaning |
|---|---|
| `listen_addr` | Socket address for the integration listener. Required. |
| `credentials_file` | Reference to the scoped integration credential document. Required in production. |
| `remote_access_enabled` | Deliberate acknowledgement that the listener may accept remote connections. |
| `tls_certificate_file` | PEM certificate chain reference. Required with a non-loopback address. |
| `tls_private_key_file` | PEM private key reference. Required with a non-loopback address. |

A loopback listener needs no TLS. A non-loopback address requires all of `remote_access_enabled`,
a complete certificate and key pair, and a credential reference; a half-configured TLS identity is
rejected rather than silently ignored.

## Integration credentials

The referenced document declares each integration principal, its bearer token, and its explicit
permission and canonical resource boundary. A principal without a permission or without a resource
scope is rejected, so adding control authority later cannot turn an unscoped observer into a
global command publisher.

```toml
[[principals]]
id = "ems-reader"
token = "secret supplied outside source control"
permissions = ["read"]
bridges = ["site-01"]

[[principals]]
id = "ems-operator"
token = "another secret"
permissions = ["read", "control"]

[[principals.stations]]
bridge_id = "site-01"
station_id = "station-a"
```

The file must not be group- or world-readable. Only `read` and `control` can be named: there is no
diagnostic, capture, or administration permission in this vocabulary, and every grant is bound to
the configured target instance. An integration credential therefore carries a target command
origin and can never be configured on the management listener, which requires a management origin.

## Endpoints

| Endpoint | Behavior |
|---|---|
| `GET /bridge/v1/capabilities` | Contract version, served resources and operations, descriptor delivery semantics, applicable limits, and the calling credential's own permissions and resource scopes. |
| `GET /bridge/v1/stations` | Paginated canonical station inventory with each station's current snapshot. |
| `GET /bridge/v1/stations/{station_id}` | One current canonical station snapshot. |
| `GET /bridge/v1/points` | Filtered, paginated canonical point catalog with descriptors and latest values. |
| `GET /bridge/v1/points/{point_id}` | One canonical point descriptor and its latest value. |

Any other path returns `ems_scada_http.unknown_resource`, and an unsupported method on a served
resource returns `ems_scada_http.unsupported_operation`. Error bodies carry a stable machine-readable
`error` code and never echo a path, payload, or credential.

### Reading canonical state

Every read requires a credential holding the `read` permission. A listener with no credential
document serves only `GET /bridge/v1/capabilities`: an anonymous local caller has no reader role
and no resource scope, so it reaches no canonical record.

| Parameter | Applies to | Meaning |
|---|---|---|
| `limit` | both lists | Page size. A zero or oversized value is refused rather than clamped. |
| `after` | both lists | Opaque cursor from the previous page of the *same* list, with the same filters. |
| `bridge_id` | items and `points` | Required only when the credential holds scopes in more than one bridge. |
| `station_id` | `points` | Restricts the catalog to one station. |
| `evse_id`, `connector_id` | `points` | Narrows a named station to one EVSE, one connector, or one connector below an EVSE. An EVSE named without a connector covers its connectors. |

`GET /bridge/v1/points/{point_id}` addresses a point inside one resource, so it requires
`station_id` and, for a point below the station, the EVSE or connector identifiers. Point
identities are stable within their owning resource, not across a bridge.

Descriptors and values are the canonical contract objects themselves. Units, access mode, exact
decimals, source and observation timestamps, quality, and freshness therefore reach an EMS client
exactly as the bridge recorded them, and are identical to the documents the MQTT target publishes.
Station-level meters — OCPP 1.6 connector zero and OCPP 2.0.1 EVSE zero — are listed against the
station itself and carry no snapshot descriptor.

Every query, list and item alike, is checked against the calling credential's own permission and
resource scopes, which are narrower than or equal to the grant the composition root gave the
target instance. Enumeration silently omits everything outside the caller's scope; a direct
identifier outside it is refused with `ems_scada_http.permission_denied` whether or not the
resource exists, so neither route can be used to discover another scope's stations.

### Read error codes

| Code | Status | Meaning |
|---|---|---|
| `ems_scada_http.invalid_request` | 400 | Malformed identifier, filter, page bound, or cursor. |
| `ems_scada_http.bridge_required` | 400 | The credential spans several bridges; name one with `bridge_id`. |
| `ems_scada_http.cursor_expired` | 400 | The cursor is outside the source's retention window. |
| `ems_scada_http.permission_denied` | 403 | The credential holds no reader role, or no scope covering the resource. |
| `ems_scada_http.resource_not_found` | 404 | The addressed resource is in scope but is not currently known. |
| `ems_scada_http.expired` | 410 | The request expired before the canonical source admitted it. |
| `ems_scada_http.operation_not_supported` | 501 | The host did not grant this query class to the target instance. |
| `ems_scada_http.capacity_exhausted` | 503 | The bounded concurrent-request budget is exhausted. |
| `ems_scada_http.source_unavailable` | 503 | Authoritative canonical state is temporarily unreadable. |
| `ems_scada_http.deadline_exceeded` | 504 | The canonical source did not answer within the listener's bounded deadline. |

## Delivery meaning

Delivery through this target means the canonical record is available on the local integration
surface. It does not assert that an EMS client consumed it, so the descriptor advertises
`local_exposure` and nothing else. Host deliveries are drained continuously, so an absent or slow
integration client cannot fill the target outbox or block charging.

## Bounded reads

The listener holds no canonical state of its own: every read is answered through the host's scoped
query port, which owns consistency and persistence. Concurrent clients, request bodies, page sizes,
and the number of station snapshots one point page may inspect are all bounded, and every canonical
read carries a deadline, so a slow authoritative source releases a client slot instead of holding
it. The applicable values are advertised under `limits` in the capability response.

## Current limits

This build terminates no TLS. A non-loopback address is refused at bind time even when its
configuration is complete, so the integration API can never be served in cleartext on a public
address.
