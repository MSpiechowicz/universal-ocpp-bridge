# MQTT target

The `mqtt` target is an outbound MQTT 3.1.1 adapter for canonical v1 bridge records. It uses one
long-lived client and continuously polls its event loop; it does not subscribe to command topics or
admit target-originated commands. MQTT command ingress is a separate capability and retained
commands are prohibited.

## Configuration

`broker_url` is the single transport endpoint. Do not add the generic `targets.transport` table to
an MQTT entry: startup rejects two independently configurable endpoints. Production uses
`mqtts://`, requires a credential-file reference, and verifies the broker certificate. WebSocket
transports and URLs containing credentials, queries, fragments, or non-root paths are rejected.

```toml
[bridge]
id = "site-01"
environment = "production"
target_id = "main"

[[targets]]
id = "main"
kind = "mqtt"
enabled = true
revision = 1

[targets.settings]
broker_url = "mqtts://broker.example:8883"
credentials_file = "/run/secrets/uob-mqtt.toml"
```

The referenced credential document can contain a username/password pair, or a client certificate
and private key together with a custom CA:

```toml
username = "bridge-site-01"
password = "secret supplied outside source control"
# ca_certificate_file = "/run/secrets/broker-ca.pem"
# client_certificate_file = "/run/secrets/bridge.pem"
# client_private_key_file = "/run/secrets/bridge-key.pem"
```

The credential document and private-key file must be regular, non-symlink files and must not grant
group or world permissions on Unix. File sizes are bounded and credential values and paths are not
copied into diagnostics.

Plaintext TCP is available only for an isolated demo and requires an explicit acknowledgement; the
environment alone never opts in:

```toml
[bridge]
id = "local-demo"
environment = "demo"
target_id = "main"

[[targets]]
id = "main"
kind = "mqtt"
enabled = true

[targets.settings]
broker_url = "mqtt://127.0.0.1:1883"
allow_plaintext = true
```

`allow_plaintext` is rejected for TLS endpoints and outside `demo`.

## Topics and delivery meaning

The composition root supplies the trusted environment and bridge ID. Neither target settings nor
payload fields can override them. Every dynamic topic segment uses collision-free percent encoding,
so `/`, `+`, `#`, `%`, controls, and non-ASCII bytes cannot create topic levels or wildcards.

| Canonical record | Topic | Retained |
| --- | --- | --- |
| Availability | `uob/v1/{environment}/{bridge}/availability` | yes |
| Station snapshot | `uob/v1/{environment}/{bridge}/state/{station}` | yes |
| Durable event | `uob/v1/{environment}/{bridge}/events/{station}/{event_id}` | no |
| Command result | `uob/v1/{environment}/{bridge}/results/{station}/{request_id}` | no |
| Redacted trace | `uob/v1/{environment}/{bridge}/traces/{station}/{trace_id}` | no |

All publications use QoS 1. A broker `PUBACK` produces `Acknowledged` with scope
`mqtt.broker_received`; it proves broker receipt only, never downstream application consumption.
The payload is the canonical compact JSON object and must carry contract version 1.0. Availability
is a small adapter-owned JSON record under the same configured payload bound. Online, offline, and
cached-state replay are internal publications and never create duplicate host delivery reports.

The retained offline availability record is also the MQTT Last Will. Graceful shutdown publishes
and acknowledges offline state before `DISCONNECT`. After a connection loss, bounded exponential
backoff and continued event-loop polling reconnect the client. A new broker session republishes
online availability and the bounded latest-state cache. A no-progress deadline also resets a socket
that remains connected while tracked publishes stop advancing; healthy idle connections do not arm
that watchdog. Event, result, trace, and any future command publication are never retained. Request,
in-flight, retained-state, payload, topic, and diagnostic buffers all have fixed bounds; an outage
leaves additional durable work in the host-owned queue.

## Broker integration test

Normal tests use a hermetic MQTT wire peer. The ignored test below additionally exercises a real
Mosquitto broker, including the exact event/result/trace topics and canonical schemas, QoS and
retain flags, environment isolation, retained reads, a forced new session, online/state
republishing, last-will availability, and graceful shutdown.

Start the pinned broker fixture from the repository root:

```text
docker run --rm --name uob-mqtt-test -p 127.0.0.1:18883:1883 -v "$PWD/adapters/target-mqtt/tests/mosquitto.conf:/mosquitto/config/mosquitto.conf:ro" eclipse-mosquitto:2.0.22
```

Then run:

```text
UOB_MQTT_MOSQUITTO_URL=mqtt://127.0.0.1:18883 cargo test --locked -p uob-mqtt-target-adapter --test mosquitto -- --ignored --nocapture
```
