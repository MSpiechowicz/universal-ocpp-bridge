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

Any other path returns `ems_scada_http.unknown_resource`, and an unsupported method on a served
resource returns `ems_scada_http.unsupported_operation`. Error bodies carry a stable machine-readable
`error` code and never echo a path, payload, or credential.

## Delivery meaning

Delivery through this target means the canonical record is available on the local integration
surface. It does not assert that an EMS client consumed it, so the descriptor advertises
`local_exposure` and nothing else. Host deliveries are drained continuously, so an absent or slow
integration client cannot fill the target outbox or block charging.

## Current limits

This build terminates no TLS. A non-loopback address is refused at bind time even when its
configuration is complete, so the integration API can never be served in cleartext on a public
address.
