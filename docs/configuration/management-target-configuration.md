# Target configuration over the management API

Configuration clients such as the browser console can read the target catalog, validate a candidate
target section, apply it for the next service start, and authorize the audited archive or discard of
undrained critical deliveries. They use the independent management listener; no CLI shell is
needed and clients don't reimplement validation.

The routes are mounted only when `[configuration_api]` configures at least one credential. Every
route requires a bearer credential whose grant is scoped to the whole bridge. Ordinary `read`,
`control` and `privileged_control` grants, and station-scoped grants, never include configuration
access.

## Service configuration

```toml
[configuration_api]
# Service-writable private file that apply replaces atomically; omit it to disable apply.
staged_targets_file = "/var/lib/uob/staged-targets.toml"

[[configuration_api.credentials]]
principal = "config-viewer"
token_file = "/etc/uob/secrets/configuration-viewer.token"
permissions = ["configuration:read"]

[[configuration_api.credentials]]
principal = "config-admin"
token_file = "/etc/uob/secrets/configuration-admin.token"
permissions = ["configuration:read", "configuration:write", "configuration:discard"]
```

| Permission | Access-policy grant | Allows |
|---|---|---|
| `configuration:read` | `ConfigurationRead` | catalog, configuration view, validate |
| `configuration:write` | `ConfigurationWrite` | apply, authorize `archive` |
| `configuration:discard` | `DestructiveDisposition` | with `configuration:write`, authorize `discard` |

Token files follow the same rules as diagnostic credentials:
- an absolute canonical path
- a single link
- no access for other users
- at most 256 bytes
- the `uob1.<environment>.` audience
- distinct from every other configured token

Principals and token paths must be unique. In staging, token files must live under
`/etc/uob-staging` and the staged file under `/var/lib/uob-staging`. Tokens are read once at start-up
and are never echoed in responses, errors or log lines.

## Routes

All responses carry `Cache-Control: no-store`. Bodies are limited to 64 KiB, at most four requests
run at once (`429 configuration.busy`), and each operation has a ten-second deadline (`504`).

| Route | Permission | Result |
|---|---|---|
| `GET /api/v1/configuration/targets/catalog` | read | Registry kinds with display family, field schema, presets, capabilities, transport policy and `available`. |
| `GET /api/v1/configuration/targets` | read | Running destination, next-start section, `configuration_digest`, `restart_required`, backlog per destination, unsettled dispositions. |
| `POST /api/v1/configuration/targets/validate` | read | Validation report for a candidate; nothing is persisted. |
| `POST /api/v1/configuration/targets/apply` | write | Persists a valid candidate for the next start. |
| `POST /api/v1/configuration/targets/dispositions` | write (+ discard) | Authorizes archive or discard of one exact old destination. |

Missing or unknown credentials get `401 configuration.unauthenticated`. A credential without the
permission gets `403 configuration.forbidden`. A malformed body or an unknown field gets
`400 configuration.invalid_request`.

### Catalog

The catalog comes from the same registry that start-up and `uob config check` validate with. It
lists `mqtt`, `ems-scada.http`, and the first-release `ems-scada.opcua`, which is marked
`"available": false`. The MQTT `ems-scada` profile is a preset of the `mqtt` kind, not a second
target.

### Configuration view

The view reads the startup file and the staged section fresh on every request:
- `next_start.source` is `base` for the startup file or `staged` after an API apply.
- Settings appear only when the kind's schema declares them.
- Credential fields appear only as `{"credential_reference": "<path or name>"}`.
- Undeclared settings, text that could embed URL userinfo (`@`), control characters and oversized
  values are replaced with `{"redacted": true}`.

### Candidate shape

```json
{
  "target_id": "api",
  "targets": [
    {"id": "main", "kind": "mqtt", "enabled": false,
     "settings": {"broker_url": "mqtts://broker.example:8883",
                  "credentials_file": {"credential_reference": "/etc/uob/secrets/mqtt.toml"}}},
    {"id": "api", "kind": "ems-scada.http", "enabled": true,
     "settings": {"listen_addr": "127.0.0.1:9080",
                  "credentials_file": "/etc/uob/secrets/ems-api.toml"}}
  ]
}
```

Settings are booleans, integers or text. As in TOML, names ending in `_file` or containing
`credential` hold references, either as plain strings or in the view's `credential_reference` form.
A candidate may declare up to 16 instances with up to 32 settings each. The generic `transport`
block isn't accepted, because no implemented kind uses it.

The service assigns revisions:
- An instance whose kind and settings match the next-start section keeps its revision.
- A changed or new instance gets a revision above every revision that instance already uses: the
  next-start, running, backlog or disposition revision. Old work therefore never matches the new
  owner.
- `enabled` is a selection flag, not a settings change.

### Validation report

Validation runs the full `uob config check` document validation (identity, staging isolation,
listener and transport policy, and factory settings) with the candidate's target section
substituted. It then runs the target-change preview against the durable outbox backlog.

The report contains:
- `valid`
- sanitized `errors`, each a stable `code` plus an optional `target_id` and schema `field`; rejected
  values are never echoed
- `assigned_revisions`
- `running_destination` and `next_destination`
- `restart_required`
- `pending_critical_deliveries`
- `blocking_destinations`: old destinations with pending critical work and no disposition
- `dispositions`: audit events covering old destinations that would otherwise block

Common codes:
- `target.unavailable_kind`, `target.unknown_kind`
- `target.missing_field`, `target.invalid_field`, `target.unknown_field` (with `field`)
- `target.credentials_required`, `target.plaintext_requires_isolated_demo`
- `configuration.invalid_transport`, `configuration.unsafe_staging_network`
- `target.pending_destination_change`

### Apply

```json
{"expected_digest": "sha256:…", "configuration": { /* candidate */ }}
```

Apply responds as follows:
- `409 configuration.conflict` when `expected_digest` no longer matches the next-start section, so a
  stale editor can't overwrite another apply.
- `422 configuration.invalid` with the same report when validation fails.
- `503 configuration.apply_unavailable` when no `staged_targets_file` is configured.
- On success, the next-start digest, the unchanged running destination, the next destination,
  `restart_required`, and the audit events that cover old destinations with pending critical
  work. The view lists every unsettled authorization.

Apply never replaces, stops or reconnects the running target. Every change takes effect only after a
service restart. `restart_required` is true whenever the next destination differs from the running
one.

The startup file stays read-only (`ProtectSystem=strict`), so apply writes the private staged file
atomically (mode `0600`, write, fsync, rename, directory fsync). The next `uob config check` and
`uob serve` load it in place of the startup file's `bridge.target_id` and `[[targets]]`. The staged
file records the digest of the startup file's own target section:
- An operator edit to that section afterwards fails start-up with `StagedTargetsConflict` rather
  than silently picking either side. Delete the staged file, or apply again, to resolve it.
- Unrelated edits to the startup file don't conflict.
- Applying a section identical to the startup file removes the staged file.

## Audited dispositions

A destination change is blocked while any old destination has pending critical deliveries without
an authorized disposition. Pending deliveries are never rerouted.

```json
{"target_id": "main", "configuration_revision": 1, "action": "archive"}
```

Authorization requires pending critical work for that exact instance and revision
(`409 configuration.disposition_not_required` otherwise). Each destination can have only one
unsettled authorization (`configuration.disposition_exists`), and at most 64 can be unsettled at once
(`configuration.disposition_limit`).

The response is the durable audit record, with `201` and these fields:
- `audit_event_id`
- `authorized_by`, the authenticated principal
- `authorized_at`
- `state: "authorized"`

That audit event is the proof that validation hands to the target-change preview.

Authorization doesn't touch the outbox. The old destination may still be the running target and may
keep admitting work until the restart. Archiving its rows under a live delivery worker would race
the worker. The next start therefore settles every authorization before any target session reads
the outbox:
- If the starting configuration no longer selects that destination, all of its pending deliveries
  are moved to `target_delivery_archive` (`archive`) or removed (`discard`). This includes work
  admitted after authorization. The audit record becomes `executed` with critical and total counts.
- If the start still selects that destination, the record becomes `superseded` and its work keeps
  draining normally.

Each settled record is logged with its audit event ID, action, destination and counts.

Archived rows count toward the operational storage budget. There's no archive read or purge route
in this release.

Work that an old running target admits between apply and restart, without an authorized
disposition, stays pending under its own destination after the restart. It appears in the view's
backlog and blocks the next change until it drains or gets its own disposition.
