# OCPP 1.6J security events and ExtendedTriggerMessage

The bridge accepts the Security Whitepaper Edition 4 `SecurityEventNotification` (A04, §5.15/§5.16,
section 8) from OCPP 1.6J stations and journals each one as a redacted, bounded station event. It
can also send the privileged `ExtendedTriggerMessage` (§5.5/§5.6), which asks a station to send
`LogStatusNotification` or a signed `FirmwareStatusNotification` (N01.FR.12, L01.FR.28/29) besides
the classes that `TriggerMessage` already covers. Both are evidence paths only: a security event
never changes configuration, policy or a command.

## Scope

- `SecurityEventNotification` is answered for every OCPP 1.6J station and needs no option. An
  OCPP 2.0.1 station still gets `NotImplemented` for it; 2.0.1 security events are #128.
- `ExtendedTriggerMessage` is a privileged, default-off, **demo-only** station option for OCPP 1.6J.
  It never sends `SignChargePointCertificate`, because that triggers a `SignCertificate` request and
  belongs to certificate management (#129).
- Both reuse the existing journal, snapshot activity and `trigger_observation` machinery. There is
  no new job table.

## Security event flow

1. The station sends `SecurityEventNotification.req` with `type` (at most 50 characters),
   `timestamp` (RFC 3339) and optional `techInfo` (at most 255 characters). The decoder
   validates the pinned schema and redacts `techInfo` while it builds the observation, so raw text
   never enters the application. A payload that fails validation (missing field, over-long text,
   extra field, wrong JSON type, non-RFC 3339 timestamp) gets CALLERROR
   `PropertyConstraintViolation`, writes nothing and leaves the session open.
2. A station without a current Accepted registration (not booted, Pending or Rejected) gets CALLERROR
   `ProtocolError` and writes nothing. This check runs first, so such a station cannot spend any
   flood budget.
3. The application looks for the same event in the journal (see Deduplication). A duplicate is
   acknowledged with `[3, id, {}]`, writes nothing and spends no budget.
4. The station's flood gate judges a new event (see Flood behavior). A shed event is acknowledged
   and not stored.
5. An admitted event is committed as one journal event and the station's snapshot activity in a
   single atomic storage write. Only then is the empty `SecurityEventNotification.conf` sent. A
   failed write gives CALLERROR `InternalError`, keeps the flood budget it spent and leaves nothing
   partial; a retry commits exactly once.

Unknown or vendor `type` values up to 50 characters are kept verbatim and never rejected. The event
is flagged `standard` only when `type` is, case-sensitively, one of the 21 names of Whitepaper
section 8.

## Contract

The journal `EventType` is `station.security_event.16`, with origin `station`, the station as
resource, and `source_time` equal to the charger's timestamp normalised to UTC. The payload is
`security_event_16`:

| Field | Meaning |
|---|---|
| `event_type` | Native `type`, verbatim, at most 50 characters |
| `standard` | `true` for the 21 section 8 names |
| `timestamp` | Charger timestamp as UTC |
| `tech_info` | Redacted `techInfo`, at most 1024 characters; omitted when the station sent none |
| `tech_info_redactions` | Number of replaced values; omitted when 0 |
| `shed_calls` | Calls acknowledged but not stored since the previous durable record; omitted when 0 |

The event is visible on `/api/v1/events` (filter with `types=station.security_event.16`) and in
`uob events`. A late subscriber replays the identical record. It is a `StationEvent` variant, which
is not part of any published schema, so it changes no schema revision.

## Persistence, deduplication and retention

A04.FR.02 obliges the charger to queue events and redeliver them, so the bridge must acknowledge a
redelivery without storing it twice. A redelivery is recognised by station, `type`, UTC timestamp
and redacted `techInfo`, whether it arrives on the same connection, after a reconnect or after a
daemon restart. The check is an existence read of the journal on the storage worker, backed by a
partial expression index over the whole key: the station stream, the timestamp, the event type and
`techInfo` (SQLite `user_version` 23; the schema refuses a newer version). The read is an exact-key
index search that examines one index entry, so a station that repeats one timestamp with varying
`techInfo`, or a charger with an unset clock, cannot slow the shared storage worker. Text is
compared as decoded JSON text with binary collation, and an absent `techInfo` equals only an absent
one, not the empty string. A `techInfo` containing an embedded NUL (`\u0000`) is handled the same way
(SQLite keeps the NUL). A lookup that fails is answered with CALLERROR `InternalError`; nothing is
written and the charger redelivers.

The record is a journal event, so it follows the seven-day journal retention. Once pruned, an event
is no longer recognised, and a redelivery of it is stored again. An event that differs only in its
timestamp offset is the same instant and the same event.

Two genuinely distinct events that share the station, `type`, timestamp and redacted `techInfo`
(for example two `MaintenanceLoginFailed` events in the same second without `techInfo`, or two
events whose `techInfo` differs only in a redacted secret) cannot be told from a redelivery. The
second is acknowledged and not stored, and it is not counted in `shed_calls`.

## Redaction

`techInfo` is untrusted charger text and the Whitepaper does not forbid credentials in it. The
bridge applies four deterministic rules, replaces each secret with `[redacted]`, keeps the
surrounding context and records how many values it replaced. The raw secret is not kept in journal
rows, the SQLite file, the event stream, traces, logs or `Debug` output.

1. The value after a secret label and a separator, case-insensitively. A label is the end of the
   identifier (letters, digits, `_`, `-`, `.`) that directly precedes the separator, so
   `dbPassword=`, `wifi.pwd:` and `x_secret=` match. The labels are `password`, `passwd`, `pwd`,
   `passphrase`, `secret`, `token`, `credential`, `credentials`, `authorizationkey`, `authkey`,
   `apikey`, `api_key`, `privatekey`, `private_key`, `idtag`, `id_tag`, `idtoken`, `id_token`,
   `totp` and `hotp`. The short labels `key`, `pin`, `pass` and `otp` also occur inside ordinary
   words, so they need a word boundary before them (start of text, a non-alphanumeric character
   or a camelCase step): `apiKey=` and `userPin:` are redacted, `monkey: 3` and `bypass=1` are
   not. A trailing plural or number is ignored (`tokens=`, `password1=`). The separator is `=`,
   `:`, `:=`, `=>`, `->`, `==`, or URL-encoded `%3D` and `%3A`, after an optional closing quote
   of the key. A quoted value runs to its closing quote and honours backslash escapes (including
   JSON carried inside a JSON string); an unquoted one runs to whitespace or one of `,;&})`.
   The value after an `Authorization` or `Proxy-Authorization` label is replaced as a whole,
   scheme and credential together.
2. The token after `Basic` or `Bearer`. A word after one of these is always redacted.
3. URL userinfo between `://` and `@`; the host is kept.
4. A run of token characters of at least 32 hex digits, or of at least 24 base64 characters that
   mix upper case, lower case and digits. This also catches long CamelCase identifiers and hex
   fingerprints, which are not secrets; over-redaction is the accepted direction.

Redacting redacted text changes nothing. The limits are real: a short secret with no label, such
as `hunter2` standing alone, is not caught; neither is natural-language phrasing such as
`password is hunter2`, a value in another encoding (double URL-encoding, for example), or the
parameters of a `Digest` header after its first two words. A short label written without a word
boundary is only caught for the listed compounds (`SECRETKEY`, `accesskey`, `SIMPIN` and similar),
so `doorpin=1234` is kept; so is a label wrapped in brackets (`config[password]=x`) and labels the
list does not know (`cookie`, `sessionid`). An unquoted value ends at whitespace or one of
`,;&})`, and the text after that character stays readable. Do not read the redaction as a
guarantee that `techInfo` is free of sensitive text. The 255-character native limit and the
1024-character record limit bound what can be kept.

## Flood behavior

Storage retention cannot shed journal events, so the bridge limits them before the write. Each
configured station has a budget of 60 journal records per 60 seconds for the life of the daemon
process; a reconnect does not refresh it. A call beyond the budget is still acknowledged, writes
nothing and raises `uob_dropped_diagnostics_total` on `/metrics` (`dropped_diagnostics` on
`/api/v1/health`). The next stored record carries `shed_calls`, counted since the last durable
record. A failed write keeps the count for the next one. Duplicates and unregistered stations never
spend budget.

The metric is shared by all stations and does not name the station or the event type. The
per-station `shed_calls` count appears only on that station's next stored record and is lost if the
station sends nothing more or the daemon restarts. A shed event was acknowledged, so the charger
drops it from its queue; a station that fills its own budget can hide its later events this way.
A backward step of the bridge clock starts a new window.

## ExtendedTriggerMessage

### Admission

A caller sends a privileged `Ocpp` command with action `ExtendedTriggerMessage`, payload schema
`urn:OCPP:Cp:1.6:2020:3:ExtendedTriggerMessage.req` and the native payload
`{"requestedMessage": ..., "connectorId"?: ...}`. The command needs:

- the privileged grant;
- the station option `extended_trigger_message = true`, which advertises the protocol action on the
  station and on each connector from 1;
- an accepted registration (except `BootNotification`, see below), a live socket and an unexpired
  command.

The descriptor lists only the `requestedMessage` values the station can answer:

| Value | Offered when | `requested_class` in the result | Result revision |
|---|---|---|---|
| `BootNotification` | Always; only before Accepted registration | `BootNotification` | 2 |
| `Heartbeat` | Always | `Heartbeat` | 2 |
| `MeterValues` | Always | `MeterValues` | 2 |
| `StatusNotification` | Always | `StatusNotification` | 2 |
| `LogStatusNotification` | The station advertises `GetLog` | `LogStatusNotification` | 19 |
| `FirmwareStatusNotification` | The station advertises `SignedUpdateFirmware` | `SignedFirmwareStatusNotification` | 19 |

`connectorId` is optional and must be positive. Station-only classes ignore it for scope.
`MeterValues` and `StatusNotification` follow the scope rules of
[`TriggerMessage`](ocpp16-remote-control.md): a positive ID must match the addressed connector, and
an omitted ID addresses all configured targets, frozen before dispatch and bounded to 64.

### Refusals

Nothing is sent when the request is refused. At the management HTTP surface:

- `SignChargePointCertificate`, a `connectorId` of zero or below, an unknown value, an extra field
  or a payload schema other than the one above gets 400 `command.invalid_schema_or_payload`. The
  value is not in the offered descriptor, so it is refused before the adapter's own
  `UnsupportedOperation` (certificate) or `InvalidParameters` (the rest) applies.
- A class whose capability is missing (`FirmwareStatusNotification` without `SignedUpdateFirmware`,
  `LogStatusNotification` without `GetLog`) gets 422 `unsupported_operation`.
- A station without the option gets 422 `command.unsupported_schema`.
- A caller without the privileged grant gets 403.

Other failures keep their existing codes: `PolicyRejected` for an unregistered station,
`StationDisconnected` and `Expired`. Plain `TriggerMessage` still refuses the two Whitepaper-only
classes.

### Dispatch and observation

A valid command sends exactly one `[2, id, "ExtendedTriggerMessage", payload]` with the native
payload. The result carries `trigger_observation` with the native reply (`Accepted`, `Rejected` or
`NotImplemented`), the target scope frozen at dispatch and a 60-second deadline.

- `Accepted` and a compatible committed station message inside the window: `observed`.
- `Rejected` or `NotImplemented`: `unsupported`.
- `Accepted` and nothing compatible by the deadline: `absent`, or `partial` when only some
  targets arrived. Only `absent` is exercised in the service tests.
- A plain `FirmwareStatusNotification` never satisfies `SignedFirmwareStatusNotification`, and the
  reverse.

The station's reply may be delayed without blocking heartbeats. A timeout or lost socket leaves the
result uncertain, and nothing is resent after a reconnect or restart. A compatible message that
arrives after a restart still reconciles inside the window. `BootNotification` is allowed only
before the registration is Accepted, as for `TriggerMessage`.

A triggered `LogStatusNotification(Idle)` without a `requestId` (N01.FR.12) and a triggered
`SignedFirmwareStatusNotification` still reconcile their log and firmware jobs by the existing
rules; they now also count as trigger evidence. Their markers are emitted only when the handler
committed.

Contract revision 19 applies only to the two Whitepaper classes. Command-result published schema is
v1.19, export-record and export-batch are v1.20.

## Configuration

```toml
[charging]
control_grant_file = "/var/lib/uob/demo-secrets/management-control"
privileged_grant_file = "/var/lib/uob/demo-secrets/management-privileged"

[[charging.stations]]
id = "demo-1"
protocol = "ocpp16j"
credential_file = "/srv/uob-demo/private/demo-1.credential"
extended_trigger_message = true
get_log = true                      # enables the LogStatusNotification class
diagnostics_job_timeout_seconds = 1800
```

`extended_trigger_message` is OCPP 1.6J only (`config check` refuses it for 2.0.1) and needs the
privileged grant. `get_log` and `signed_update_firmware` enable the `LogStatusNotification` and
`FirmwareStatusNotification` classes and bring their own requirements (a `[charging.firmware]`
section and a job timeout; see [diagnostics](ocpp16-diagnostics.md) and
[firmware](ocpp16-firmware.md)). `SecurityEventNotification` needs no option. See
[the headless CLI guide](../operations/headless-cli.md#protected-ocpp-16-security-events-and-extendedtriggermessage)
for the operator view.

## Not covered

- `AuthorizationKey` rotation (A01).
- Security profiles and their transitions (A00, A05), tracked in #345 and #346.
- Certificate messages, `SignChargePointCertificate` and `SignCertificate` (#129).
- OCPP 2.0.1 security events (#128).
- The charger-side security log (A04.FR.04). Log retrieval is in [diagnostics](ocpp16-diagnostics.md).
- Credential detection beyond the four redaction rules.
- Service-level proof of the storage-failure `InternalError` path (covered in the application
  tests), a `partial` observation, and the `uob events` command (the HTTP stream is covered).
- Physical stations, hardware interoperability and OCA certification.

## Verification

```text
cargo test --locked -p uob-contracts --test security_event16
cargo test --locked -p uob-application --lib security_event16
cargo test --locked -p uob-application --test security_event16
cargo test --locked -p uob-storage-adapter --test security_event16 --test schema_v23 \
  --test trigger_observation
cargo test --locked -p uob-protocol-adapter --test command_registry \
  --test ocpp16_security_event_decode --test ocpp16_extended_trigger
cargo test --locked -p uob-service --test security16
cargo test --locked -p uob-sim --test security16_model --test security16_wire \
  --test security16_trigger_wire --test security16_process --test security16_scenario \
  --test security16_independence
cargo test --locked -p uob-ocpp-fixtures --test security16
cargo build --locked -p uob-service -p uob-sim
python3 bins/uob-sim/tests/security16_joint_smoke.py --bridge target/debug/uob \
  --simulator target/debug/uob-sim --output <fresh private directory>
```

Run the new test binaries with `--workspace --test <name>` as well, because the workspace enables
serde_json `arbitrary_precision` and a `-p` run can hide failures.

The joint smoke is opt-in and is not part of `scripts/verify-workspace.sh`. It starts a real daemon
with the demo artifact service and two independent simulator stations. The first station raises a
standard event, a vendor event and one whose `techInfo` carries a credential, and resends each one
after its acknowledgement. The smoke checks:

- six sends become three journal records, the credential is redacted, and a late subscriber replays
  the identical records;
- `Heartbeat` and `LogStatusNotification` triggers are observed and a rejected `StatusNotification`
  is `unsupported`;
- certificate, connector 0, extra-field, wrong-schema, plain-`TriggerMessage`, missing-capability
  and unprivileged requests are refused;
- the second station accepts a trigger but sends nothing, which ends `absent` at the deadline (60
  to 90 seconds);
- no log or SQLite file holds the station secret or the event credential.

The station model is described in [the simulator guide](../simulator/security16.md).
