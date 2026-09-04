# MQTT target

The `mqtt` target is a bidirectional MQTT 3.1.1 adapter for canonical v1 bridge records and
commands. It uses one long-lived client, continuously polls its event loop, and subscribes only to
the configured bridge's command namespace. Retained commands are prohibited.

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
home_assistant_discovery = true
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
| Command request | `uob/v1/{environment}/{bridge}/commands/{station}/{request_id}` | prohibited |
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

## Command ingress

The adapter subscribes at QoS 1 to exactly
`uob/v1/{environment}/{bridge}/commands/+/+`. A publisher sends a non-retained JSON object with
`schema_version`, `request_id`, optional `correlation_id`, canonical `resource`, typed `operation`,
and `expires_at`; trusted `origin`, environment, and admission timestamps are not payload fields.
The encoded station and request-ID topic segments must exactly match the canonical payload, and the
payload bridge must match the process configuration.

The adapter derives the origin as target instance `{target}` and principal `mqtt-target:{target}`.
Broker credentials and ACLs authorize publication to the command namespace; payload fields can
never select a principal, environment, bridge, or wider resource scope. The host's scoped command
port then reapplies operation, resource, payload-size, concurrency, authorization, safety,
capability, expiry, and durable idempotency checks.

Retained, expired, wrong-scope, unsupported, unauthorized, unsafe, malformed, oversized, and
conflicting commands are never dispatched. Rejections that contain a valid correlation identity
produce a canonical non-retained result. Identical QoS 1 redelivery reuses the application's stored
result, while reuse of a request ID with changed content produces a stable conflict result. A
protocol response remains distinct from later observed charging effects. Command admission and
protocol polling continue while outbound delivery waits for broker acknowledgement, within the
configured command and request bounds.

## EMS/SCADA preset

`profile` selects a catalog preset of this same adapter. It accepts `standard` (the default) and
`ems-scada`. A preset is not a second target: the service still starts exactly one `MqttTarget`,
and selecting `ems-scada` never starts the EMS integration HTTP listener. Conversely, a direct
HTTP target configuration starts no broker connection or MQTT task. Unimplemented drivers such as
`ems-scada.opcua` are rejected as an unsupported `profile`; they are separate adapters with their
own kind and schema.

```toml
[targets.settings]
broker_url = "mqtts://broker.example:8883"
credentials_file = "/run/secrets/uob-mqtt.toml"
profile = "ems-scada"
```

The preset adds a retained canonical point catalog beside the existing state, event, result, and
trace topics. Every other topic, payload, bound, and command rule stays exactly as documented
above.

| Canonical record | Topic | Retained |
| --- | --- | --- |
| Point descriptor | `uob/v1/{environment}/{bridge}/points/{station}/{point}` | yes |
| Latest point value | `uob/v1/{environment}/{bridge}/values/{station}/{point}` | yes |

Both payloads are the canonical `DataPointDescriptor` and `DataPointValue` documents themselves, so
they match the published v1.0 contract schemas that the direct HTTP target serves. Engineering
units, quality level and reason, freshness, `source_time`, `observed_at`, and the exact original
measurement text are carried inside those canonical records and are never re-derived, rounded, or
renamed by this adapter. Descriptors come from the canonical snapshot's per-resource data points, so
a resource belonging to another bridge or station is rejected instead of published.

Commands are unchanged: an EMS/SCADA client publishes explicit, non-retained requests to the same
authorized `commands/{station}/{request_id}` namespace, they pass through the same application
admission pipeline as direct HTTP commands, and correlated results appear under `results`. The
catalog is bounded by `point_catalog_capacity`; an over-capacity snapshot is refused as a whole and
reported as `mqtt.point_catalog_capacity` rather than published in part. After a lost broker
session the bounded catalog cache is republished with availability and current state.

The preset advertises the `ems-scada-point-catalog` capability and adds the `LocalExposure`
delivery semantic to the descriptor. Broker connectivity and a QoS 1 `PUBACK` therefore remain
evidence of broker receipt only. EMS-client presence and processing stay unknown unless the
application observes explicit evidence, such as a correlated command and its later observed
effects.

`home_assistant_discovery` still defaults to `false` under this preset; the industrial catalog and
consumer discovery are independent settings.

An EMS/SCADA system that speaks only a vendor HTTP API cannot consume these topics merely because a
broker exists. That integration needs an explicit connector implementing that API's own mapping and
authentication contract; the bridge performs no implicit MQTT-to-HTTP conversion.

## Home Assistant discovery

`home_assistant_discovery` is optional and defaults to `false` in every environment, including
staging. Enabling it publishes retained discovery configurations under the standard
`homeassistant/<component>/<node_id>/<object_id>/config` prefix for station connectivity and the
current telemetry points present in canonical station snapshots. Entity, device, node, and object
identifiers include the trusted environment, bridge, and station identity using a collision-free
encoding. Discovery payloads contain canonical state and availability topics only; they never copy
broker credentials, authorization values, command payloads, or diagnostic details.

The adapter retains the discovery configurations, bridge availability, and latest canonical
station snapshots. A Home Assistant MQTT integration restart therefore receives configuration and
current state from the broker. If the broker loses its session or retained store, the adapter's
bounded in-memory discovery and state caches are republished after reconnect. This recovery is not
durable across a simultaneous broker and bridge restart; the normal durable snapshot delivery must
repopulate the caches in that case.

Home Assistant controls must use explicit, non-retained command publications. This script creates
a fresh request ID, sets a short expiry, and does not treat publication or the later OCPP response
as evidence that charging started:

```yaml
script:
  uob_start_station_7:
    sequence:
      - action: mqtt.publish
        data:
          qos: 1
          retain: false
          topic: >-
            uob/v1/production/site-01/commands/station-7/{{ context.id }}
          payload: >-
            {"schema_version":{"major":1,"revision":0},
             "request_id":"{{ context.id }}",
             "correlation_id":"ha-{{ context.id }}",
             "resource":{"bridge_id":"site-01","station_id":"station-7"},
             "operation":{"kind":"start","parameters":{}},
             "expires_at":"{{ (now() + timedelta(minutes=1)).isoformat() }}"}
```

Observe the correlated lifecycle separately on
`uob/v1/production/site-01/results/station-7/<request-id>`. A lifecycle with
`stage: protocol_response` and `accepted: true` means only that the charger accepted the OCPP
request. Confirm the physical effect from a subsequent transaction/status event under
`uob/v1/production/site-01/events/station-7/+` or from the retained station snapshot. Automations
must never publish commands with `retain: true`, reuse a fixed request ID, or infer charging from a
result alone.

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
