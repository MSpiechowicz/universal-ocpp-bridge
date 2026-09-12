# Staging data isolation

Staging uses synthetic fixtures by default. An explicitly admitted import is a **lossy status
projection**, never a copy of a production capture, station snapshot, SQLite database, journal,
outbox, credential file or export batch. The projection is loaded only by the separately packaged
simulator; neither the production daemon nor its export provider links the importer.

## Separate PostgreSQL database and role

`packaging/postgresql/staging.sql` provisions the fixed `uob_staging` database, a separate
`uob_staging` role, and a small staging-only observation table. Run it once using an administrator's
`psql -X --set ON_ERROR_STOP=1` session. It intentionally fails if names already exist. A failed
provisioning attempt leaves the new role unable to log in; investigate rather than adopting or
blindly deleting existing roles/databases. Do not run this installer against an existing staging
schema expecting a migration.

The login role owns no database/schema/table, has no memberships, cannot create roles or databases,
and has no superuser, replication or RLS bypass privileges. PUBLIC receives no staging database,
schema or table access. The role receives only CONNECT, schema USAGE, and SELECT/INSERT on the
staging observation table. Statement and idle-transaction timeouts bound test work. This table is
a test sink, not the future production PostgreSQL exporter schema.

Before enabling login, install `packaging/postgresql/staging.pg_hba.conf` **before every broader
allow rule**, including Unix-socket trust/peer rules. Reload and check `pg_hba_file_rules` for
errors and ordering. The rules allow this exact role/database pair through SCRAM-authenticated
Unix sockets or TLS loopback, then explicitly reject the role for all other databases, addresses
and replication. Database privileges alone are insufficient when another database grants CONNECT
to PUBLIC. Likewise NOINHERIT is not a substitute for absence of role memberships.

These rules follow PostgreSQL's [first matching HBA record semantics](https://www.postgresql.org/docs/17/auth-pg-hba-conf.html)
and [role privilege controls](https://www.postgresql.org/docs/17/sql-createrole.html).
Never grant this role another membership or expose production foreign tables/functions inside the
staging database. Retain production privileges and normal production connection rules after the
staging fence. Do not put production credentials into staging configuration.

After checking the rules, use the administrator's interactive `\password uob_staging` command to
set a newly generated staging-only secret, then `ALTER ROLE uob_staging LOGIN`. Store it only in
protected staging secrets. Authenticate as that role and verify a staging insert succeeds while
production SELECT/INSERT and SET ROLE fail. Revoke LOGIN if any verification fails. For TCP clients,
configure server TLS and client certificate/hostname verification; the installer contains no
passwords or TLS keys.

The same database server may contain production and staging databases: HBA fences all production
databases, including PUBLIC-granted tables. This is a server-side provisioning policy, not an
exception to the [staging network boundary](environment-network-isolation.md). The packaged daemon
still rejects exporter settings and creates no provider work. Do not add a host port proxy or
network uplink to reach a shared server. Runtime exporter composition and any approved shared-server
transport remain separate work. A dedicated admitted test server remains the isolated profile.

## Explicit sanitized import

Use the existing simulator control catalog with an administrator-reviewed file:

```toml
[[scenarios]]
id = "sanitized-status-check"
path = "sanitized-status.json"
sanitized_import = true
```

The control configuration must use `environment = "staging"`. Its separate simulator configuration
supplies literal-loopback test endpoints, `staging-` station identities, connector/EVSE topology,
and no credential files. Imports cannot supply or override endpoints, authentication or identity.
Loading validates the complete file before admitting any scenario; it opens no station connection.

The version 1 JSON projection is:

```json
{
  "schema_version": 1,
  "kind": "sanitized_capture",
  "records": [
    {"station_slot": 0, "connector_slot": 0, "status": "Unavailable"}
  ]
}
```

`kind` is `sanitized_capture` (ordered observations within each station) or `sanitized_snapshot`
(one observation per resource). Slots are zero-based positions in the trusted simulator's station
and connector lists; OCPP 2.0.1 flattens the configured EVSE/connector pairs. They are **new test
mappings**, not original station or connector identities. Only `Available`, `Unavailable`, and
`Faulted` are admitted. Bounds are 64 KiB and 1–32 records, plus the existing catalog, step, run,
queue and deadline limits. Duplicate snapshot resources, unsupported versions, unknown fields at
either level, malformed content and unmapped slots fail closed with fixed errors.

Prepare this projection explicitly after reviewing and sanitizing the source offline. No automatic
sanitizer claims to recognize secrets inside arbitrary free text. The schema admits no free-text
payload, source ID, timestamps, customer/payment data, transaction, command, credentials, SQL,
journal cursor, pending export identity or destination. Raw exported captures are intentionally
rejected; the inert offline full-capture inspector remains separate work. Charging/transaction
replay, original timing, raw wire fidelity and production export recovery are not supported by this
status-only format. Unsupported information must be omitted deliberately, not silently relabeled.

Starting an imported catalog entry checks the actual root-owned `/run/netns/uob-staging` namespace
and its loopback-only interfaces/routes **before creating a run or connecting**. It generates fresh
UUID-based step and report-event identities on each run, even with the same seed or after restart.
It emits synthetic boot, status and disconnect operations through the existing simulator runner,
using a synthetic year-2000 timestamp where required. There is no authorization, transaction,
remote-command, export-delivery or database-write action. Downstream test-peer observations therefore
create only new staging evidence; production pending exports are never transferred. Imported
records cannot be fed to the general CLI runner as an executable scenario file.

Run the simulator under a separate unprivileged staging test-peer unit with the namespace,
filesystem, no-new-privileges, capability and slice restrictions in the network isolation guide.
Production state, secrets, journal and spool paths remain inaccessible under the existing
[filesystem isolation policy](environment-filesystem-isolation.md). Never copy a production SQLite
file and its identity marker into a staging directory or rewrite its marker to bypass admission.

## Verification

- `cargo test --locked -p uob-sim` checks strict import rejection, staging-only configuration and
  refusal to replay on the host network. It preserves existing synthetic scenario behavior.
- `scripts/test-staging-network.sh` runs the real WebSocket replay test inside the disposable
  namespace for OCPP 1.6J and 2.0.1. Repeated runs use distinct evidence identities and emit only the
  exact synthetic boot/status payloads. The existing namespace suite proves host sockets are
  unreachable and existing distinct-UID checks protect production state and secrets.
- `scripts/test-staging-postgresql.sh` starts a pinned disposable PostgreSQL 17 container without
  host network, published ports or mounted data. It applies the shipped SQL/HBA rules, authenticates
  a fresh staging credential, proves permitted inserts and denied production access, privilege
  escalation, DDL, wrong-password and production-environment writes, and verifies the production
  canary is unchanged. It never contacts an installed database.

CI runs both isolation scripts. Privileged namespace checks belong on disposable Linux runners,
not charging hosts. Run `scripts/verify-workspace.sh` for the full ordinary repository checks.
