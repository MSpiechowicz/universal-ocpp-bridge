# Staging network isolation

The packaged staging profile uses the root-owned named network namespace
`/run/netns/uob-staging`. It contains only loopback: no host loopback, physical interface,
veth, route to a LAN, DNS service, or forwarded production port. Production remains in its
normal network namespace. A test peer is admitted by an administrator explicitly starting
it inside the staging namespace; the staging UID cannot add interfaces or join another namespace.
This profile works on the same Pi or a separate Linux test host.

`uob-staging.service` requires and binds to `uob-staging-network.service`, which creates a
fresh namespace and brings up loopback. An existing namespace is rejected rather than adopted.
Missing namespace support or privileges fails startup. `NetworkNamespacePath=` is mandatory;
`PrivateNetwork=yes` alone is insufficient because systemd can degrade that setting when
namespacing is unavailable. The daemon also verifies its namespace identity and refuses any
unexpected interface or non-loopback route before opening storage or listeners. Kernels can
create inert fallback tunnel devices in every fresh namespace; these are accepted only with
no non-loopback routes and no underlying network interface. The `events` CLI applies
that same runtime check before connecting. Offline `config check` validates configuration
without requiring a live namespace or modifying it.

The helper and namespace configuration are trusted, root-owned deployment policy. Never add
uplinks, host-network proxies, inherited production sockets, or production peers to this
namespace. Staging has no network-administration capabilities and cannot change that policy.
Filesystem isolation separately blocks production credential files and pathname control sockets.
The service's `RestrictNamespaces=yes` prevents it from creating or entering another namespace.
The helper has no boot install target; production has no dependency on either staging unit.

## Application checks and endpoints

Staging configuration requires a synthetic `test-` bridge ID, loopback listeners, and
credential/TLS file references under `/etc/uob-staging/` without parent-directory traversal.
Do not copy production credentials or production database contents into that directory.
Root-owned configuration and the existing inaccessible production paths prevent service-side
substitution. The target adapter derives its client ID and topic namespace from the trusted
bridge/environment identity, rather than accepting a production client ID in settings.

| Surface | Staging profile |
|---|---|
| Management | `127.0.0.1:18080` by default; production's 8080 is rejected |
| Direct EMS/SCADA target | Literal loopback on port 19080 |
| Dedicated test MQTT broker | `mqtts://127.0.0.1:18883`, separate credentials and TLS files required |
| Events CLI | Literal loopback, never production's 8080; runs inside the namespace |
| OCPP and simulator | Reserve distinct test ports when composed; no charger listener is currently started by the daemon |
| Export / payment providers | No production provider or export setting is accepted by the current strict daemon configuration |

The executable remains management-only. Target validation is available, but these changes do
not compose charging or target tasks. Unknown OCPP/payment settings are rejected, not silently
activated. Future composition must retain the same namespace proof, admit synthetic station
identities with separate credentials, and use the existing environment-scoped command guards.

A shared production broker is **not an available mode in this isolated deployment profile**.
Remote brokers, hostnames, production broker ports, missing credentials, and alternate transport
blocks fail configuration validation. Even a host broker listening on the test port is unreachable
from staging. Consequently staging cannot publish or subscribe to its production topics or
Home Assistant discovery. Naming topics `staging` or supplying an assertion that ACLs exist cannot
bypass the network boundary. A future shared-broker profile must independently verify separate
credentials/client IDs and broker-enforced denial of production topics/discovery, together with
a reviewed host network allowlist, before it can be enabled. Do not bridge this namespace to a
shared broker as an installation shortcut.

## Installation and test peers

Install the filesystem layout first, then these additional root-owned files:

```sh
install -d -m 0755 /usr/local/libexec
install -m 0755 packaging/network/uob-staging-network /usr/local/libexec/uob-staging-network
install -m 0644 packaging/systemd/uob-staging-network.service /etc/systemd/system/uob-staging-network.service
install -m 0644 packaging/systemd/uob-staging.service /etc/systemd/system/uob-staging.service
systemd-analyze verify /etc/systemd/system/uob-staging-network.service /etc/systemd/system/uob-staging.service
systemctl daemon-reload
systemctl start uob-staging.service
```

Requires Linux network namespaces, systemd with `NetworkNamespacePath=` support, and iproute2.
The shipped helper creates only the fixed `uob-staging` namespace; it does not edit host interfaces,
routes, firewall rules, or production units. It refuses an existing namespace for operator review.
Stop staging and all test peers before inspecting/removing a stale namespace. Do not delete a
namespace that still has peer processes attached.

Run each approved simulator, broker, EMS client, or local test provider under its own unprivileged
unit with these additional properties, alongside its normal filesystem/security restrictions:

```ini
[Unit]
Requires=uob-staging-network.service
After=uob-staging-network.service
BindsTo=uob-staging-network.service

[Service]
Slice=uob-staging.slice
NetworkNamespacePath=/run/netns/uob-staging
RestrictNamespaces=yes
NoNewPrivileges=yes
CapabilityBoundingSet=
```

Use separate test credentials, and keep production paths inaccessible in every peer unit.
Run an interactive test client with an explicit administrator namespace entry and then drop
privileges, for example `ip netns exec uob-staging runuser -u uob-staging -- COMMAND`.
The host browser cannot directly reach staging loopback. Use an explicitly admitted test client
inside the namespace; no host port proxy is shipped. Stop `uob-staging-network.service` to stop
the bound staging service and peer units and release the namespace. Resource limits and pressure
shedding remain tracked separately in #148.

## Verification

`./scripts/verify-workspace.sh` checks configuration rejection, runtime fail-closed parsing,
packaged directives, and rejection of an actual staging daemon started on the host network.

`./scripts/test-environment-isolation.sh` additionally runs
`./scripts/test-staging-network.sh` on a disposable Linux runner with root/sudo, iproute2, jq,
mount/unshare, and systemd tools. It builds test binaries before elevation, hides host `/run`
inside a private mount namespace, and runs the shipped helper. It fails on missing privileges
rather than silently skipping verification. It proves:

- Existing namespace adoption is refused.
- Synthetic host management/control/broker sockets receive no staging connections, including a
  broker on the allowed test port; external IPv4/IPv6 connections also fail.
- Real test-peer sockets communicate inside staging, and the actual daemon runs as UID 60002.
- An unexpected network interface prevents daemon startup.
- The existing distinct-UID filesystem and lifecycle checks still pass inside the isolated network.

Do not run privileged acceptance tests on a charging host. These tests establish software
isolation, not measured Raspberry Pi capacity or complete charging-workflow readiness.
