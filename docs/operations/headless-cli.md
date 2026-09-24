# Headless service and release CLI

The production binary is `uob`. Every command is noninteractive: it never reads a prompt from
standard input and never launches a browser. Service diagnostics go to standard error; commands
that produce machine-readable records use standard output only.

## Configuration

Both startup and offline validation read the same strict TOML document. A minimal API-only demo is:

```toml
[bridge]
id = "site-01"
environment = "demo"

[management]
listen_addr = "127.0.0.1:8080"
```

`environment` defaults to `production`, and the management listener defaults to
`127.0.0.1:8080`. The current service fails closed on non-loopback management listeners because
the TLS listener is not yet composed. Unknown sections and fields are rejected.

When a target is configured, `bridge.target_id` must name the single enabled `[[targets]]` entry.
Target `settings` are converted to the shared typed configuration boundary; names ending in
`_file` or containing `credential` are credential references, not inline secret values. The
registry still rejects kinds whose concrete factory is not present in this build.

The concrete outbound MQTT target, its TLS/plaintext rules, topic taxonomy, and broker integration
test are documented in [`MQTT target`](../configuration/mqtt-target.md). The direct integration
listener is documented in
[`EMS/SCADA HTTP target`](../configuration/ems-scada-http-target.md).

## Demo charging station views

To display real station observations, explicitly extend an isolated **demo** configuration.
Both listeners must bind loopback; plaintext charging ingress is rejected outside `demo`.
The read grant is independent of charger credentials and authorizes only the configured
station roster, not diagnostics capture, commands or an outbound target:

```toml
[bridge]
id = "local-demo"
environment = "demo"

[management]
listen_addr = "127.0.0.1:8080"

[charging]
enabled = true
listen_addr = "127.0.0.1:9000"
state_directory = "/var/lib/uob/demo-private"
read_grant_file = "/var/lib/uob/demo-secrets/management-read"

[[charging.stations]]
id = "station-a"
protocol = "ocpp16j"
credential_file = "/var/lib/uob/demo-secrets/station-a"

[[charging.stations.resources]]
connector_id = "connector-1"
native_connector_id = 1

[[charging.stations]]
id = "station-b"
protocol = "ocpp201"
credential_file = "/var/lib/uob/demo-secrets/station-b"

[[charging.stations.resources]]
evse_id = "evse-1"
native_evse_id = 1

[[charging.stations.resources]]
evse_id = "evse-1"
connector_id = "connector-1"
native_evse_id = 1
native_connector_id = 1

[[charging.stations.resources]]
evse_id = "evse-2"
native_evse_id = 2

[[charging.stations.resources]]
evse_id = "evse-2"
connector_id = "connector-2"
native_evse_id = 2
native_connector_id = 1
```

Create the state directory owned by the service account with mode `0700`; create
distinct credential files owned by the same account with mode `0600`, no symlinks
or hard links. Generate independent random station secrets of at least 16 bytes.
The read file must contain one complete `uob1.demo.`-prefixed bearer token with
32–128 printable secret characters and no newline. Protect the directory containing
those files; never put real credentials in TOML, version control, process arguments,
browser URLs or logs. Supply each station's credential through its OCPP WebSocket
Basic-auth handshake and select `ocpp1.6` or `ocpp2.0.1` accordingly.

`uob serve` verifies privacy, credentials, topology, bridge identity and exclusive
SQLite ownership before admitting stations. A matching identity marker left by an
interrupted first start can be resumed; an unmarked existing database or a foreign
bridge marker cannot be adopted. Restart retains recent transactions, points and event
cursors; transport disconnect emits a durable change and marks availability unknown
until a fresh accepted status observation. OCPP 2.0.1 retains at most 128 transaction
rows, preserving active rows and a monotonic seven-day replay floor for aged ended rows.
If the protected recent/active history fills that bound, further starts fail closed rather
than evicting a transaction or accepting an old replay. Storage retention runs at startup
and every minute while charging is active.
The management console on `http://127.0.0.1:8080` requires manual entry of the read
token and shows only committed observations. A report of a transaction is not proof
of authorization or physical charging: with no provisioned local authorization grant
the demo replies `Invalid`. Disabling `[charging]` leaves station/event reads
unavailable (503) rather than serving a synthetic inventory. Do not forward plaintext
charging ingress off the loopback host.

## Commands and exit codes

Validate without binding a socket, resolving DNS, reading credentials, or starting adapters:

```text
uob config check --config bridge.toml
```

Production release preflight can additionally read and validate secret references with
`uob config check --config bridge.toml --secrets`. See
[production preflight](release-preflight-backup.md) for reference restrictions and limits.

Success writes one JSON object to stdout:

```json
{"status":"valid"}
```

Start the service with the optional browser entry asset enabled or disabled:

```text
uob serve --config bridge.toml
uob serve --config bridge.toml --no-ui
```

`--no-ui` removes only the static browser route. Health, identity, metrics, and every management
API route remain mounted. The process does not open a browser in either mode.

Consume the configured management event stream as newline-delimited JSON:

```text
uob events --format jsonl
uob events --config staging.toml --after uob:event:41 --format jsonl
```

Without `--config`, `events` reads `bridge.toml`. Its default endpoint is the configured local
management listener at `/api/v1/events`. An explicit remote endpoint must use HTTPS and provide a
`credentials_file`; the file is read only when `events` connects and its trimmed content is sent
as a bearer credential. Redirects and proxy use are disabled so credentials stay bound to the
validated endpoint. SSE `data` records must be bounded valid JSON and are re-encoded as compact
JSONL. A server response that reflects the bearer value is rejected before it reaches stdout.

Exit code `0` means the requested operation completed successfully. Exit code `2` identifies
invalid arguments or configuration. Exit code `1` identifies runtime, network, stream, or output
failure. Diagnostics use stable sanitized categories and do not reproduce rejected configuration
values, credential contents, response bodies, or filesystem paths.

## Independent release control

Release commands connect directly to the independent supervisor's protected Unix socket.
They do not load `bridge.toml`, contact the management API, or start a bridge process:

```text
uob release stage --bundle /srv/delivery/candidate
uob release qualify --release DIGEST --evidence evidence.json
uob release promote --release DIGEST
uob release rollback --to previous-good
uob release status --format json
uob release events --format jsonl
uob release events --format jsonl --after 41
```

Every form accepts `--socket PATH`; the default is
`/run/uob-release-manager/control.sock`. Use only an administrator-controlled socket.
The supervisor authenticates the process's kernel UID, not a CLI-supplied credential.
Group membership permits connecting but does not confer an operation grant:

| Commands | Required supervisor permission |
|---|---|
| `status`, `events` | `read` |
| `stage`, `qualify` | `stage` |
| `promote`, `rollback` | `activate` |

`DIGEST` is the exact 64-character lowercase SHA-256 artifact identity, not a release label.
`--bundle` takes a directory in the existing
[signed-artifact format](signed-artifact-store.md), not a platform-package tarball.
The CLI reads its bounded `manifest.json` to select the digest. The administrator must first
install the signed manifest, signature and payload with the existing
`uob-release-manager install` command. `stage` asks the supervisor to reverify that installed
candidate's signature, bytes and eligibility; it does not install local files or claim that a
staging workload ran.

`--evidence` identifies exact document bytes, bounded to 64 KiB. Before calling `qualify`, the
administrator or trusted delivery pipeline must provision those bytes and their detached
signature in the supervisor's [protected evidence inbox](release-qualification.md).
The CLI hashes the local file; only its digest crosses IPC. It cannot supply a trusted key,
upload unsigned claims, write the inbox, or override the configured qualification matrix.
Manifest/evidence inputs must be regular files; final-component symlinks and FIFOs are rejected.

The supervisor remains authoritative. Missing or invalid installation/evidence returns policy
errors such as `artifact_rejected`, `evidence_rejected`, or `qualification_required`.
Qualified promotion uses the existing preflight gate and returns `preflight_rejected` or
`activation_blocked` where production admission/process control is unavailable through IPC.
Rollback likewise returns the existing `qualification_required` policy response.
Neither command bypasses compatibility, drain, or activation ownership. These CLI forms do not
turn the separate internal activation/automatic-rollback APIs into unconditional operator actions.

Status and mutation commands emit one JSON response containing `protocol`, `manager_version`,
and `code`, with status evidence where available. Supervisor policy failures retain their
machine-readable response and exit `1`; they are not reported as success.
Local failures emit `{"error":"..."}` with a stable sanitized category. Invalid arguments exit
`2`; input, transport, protocol and output failures exit `1`. Diagnostics remain on stderr.
Connect/write waits are bounded to one second each; the absolute response deadline is six
minutes, allowing the supervisor's configured preflight to take up to five minutes.
A timeout does not cancel supervisor work. Inspect status/events before deciding whether to retry.

Release events are a finite snapshot, not the management SSE stream or a follow mode. Each
retained record is one JSON line with `sequence`, `uid`, digest-only `request`,
`result`, and `actor` (`operator` or `supervisor`). Internal decisions also carry
safe typed `decision` evidence for compatibility/drain, health and rollback.
Operator UIDs come from socket peer credentials; internal decisions use the
supervisor's effective UID. The final line is:

```json
{"type":"metadata","cursor":42,"truncated":false,"oldest_sequence":1,"latest_sequence":42}
```

`--after` is an exclusive unsigned sequence cursor covering both actors. Retention
is at most 64 records and may be lower to keep the combined state below 64 KiB.
`truncated:true` means the requested cursor predates available history; archive output
externally if a longer history is required. Current incident context remains pinned
in status independently of event eviction. Older records without an actor are
operator records. Reads do not change the ledger. Unauthorized/malformed requests
rejected before recording are not audit entries.

Run `./scripts/test-release-cli.sh` for real CLI-to-supervisor checks with no bridge/API running,
including read-only permissions, untrusted evidence, candidate selection, restart,
cursor retrieval, and internal audit export after automatic rollback and bridge death.
It builds `uob` separately and runs the cross-binary regression cases; the workspace
verifier invokes it automatically.
