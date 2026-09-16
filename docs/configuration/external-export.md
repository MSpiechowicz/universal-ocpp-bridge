# External export configuration

External database export is optional and independent of the selected bridge target. Disabling it
creates no provider, connection, polling task, or export queue. Enabling it selects exactly one
stable provider instance; the first concrete provider kind will be `postgresql`.

The safe PostgreSQL configuration shape is:

```toml
[data_export]
enabled = true
id = "analytics"
kind = "postgresql"
revision = 4

[data_export.settings]
host = "database.example"
port = 5432
database = "charging_production"
schema = "uob"
credentials_file = "/etc/uob/production/secrets/postgresql.toml"
tls_mode = "verify-full"
```

`schema` must name the bridge-owned schema accepted by the provider. Credentials are a protected
file reference resolved only when the provider starts; passwords, connection strings, raw SQL, and
database command fields are not configuration or management API surfaces. Production and staging
require TLS with certificate and hostname verification plus a credential reference. Only an
explicitly isolated demo may relax those transport requirements.

Every pending batch and checkpoint carries both the stable provider ID and immutable configuration
revision. A restart may continue using that exact destination. Changing the ID, revision, or
disabling export is rejected while records remain pending unless they first drain. Destructive
discard is a separate authorized operation: validation requires a durable audit-event ID naming the
exact old destination, and returns that proof to the host for journaling. Pending records are never
silently relabeled or sent to a newly configured database.

The provider catalog may advertise a recognized but unavailable kind so configuration clients can
distinguish “not installed in this executable” from an unknown kind. Catalog schemas never include
credential contents. The service currently reserves PostgreSQL this way; its later driver will use
the same offline registry and validation boundary.

## Explicit PostgreSQL schema provisioning

The administration command is available independently of the runtime driver:
[`scripts/provision-postgresql.sh`](../../scripts/provision-postgresql.sh).
It provisions the canonical destination schema; it does **not** enable the currently unavailable
PostgreSQL provider, create an export scheduler, or implement conflict-content comparison.
The daemon never runs migrations or receives the administrator's credentials.

Use PostgreSQL 17 and its `psql` client. Create a dedicated destination database as a trusted
administrator, outside the daemon. For example, in an administrator `psql -X` session:

```sql
CREATE DATABASE charging_production TEMPLATE template0;
\connect charging_production
REVOKE ALL ON DATABASE charging_production FROM PUBLIC;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
```

Configure an administrator-only libpq service named `uob-export-admin` with the intended host,
database, administration role, `sslmode=verify-full`, and trusted CA. Use protected libpq
credential files rather than putting passwords or connection strings into command arguments.
The administrator needs permission to create roles and schemas; it must remain the owner used
for later upgrades. Then run from the repository:

```sh
PGSERVICE=uob-export-admin ./scripts/provision-postgresql.sh install uob uob_export_runtime
PGSERVICE=uob-export-admin ./scripts/provision-postgresql.sh upgrade uob uob_export_runtime
```

`install` refuses existing role or schema names. It creates a fresh `NOLOGIN`, non-superuser,
non-inheriting runtime role with no database/role creation, replication, or RLS-bypass authority.
Identifiers are quoted as data and must fit PostgreSQL's 63-byte identifier limit.
Provisioning runs in one transaction: an error rolls back both role and schema changes.
It never silently adopts existing objects or adjusts unrelated database privileges.

Before enabling login, install ordered `pg_hba.conf` rules allowing the runtime role only its
destination database, approved source address, and verified TLS/SCRAM or certificate credentials;
reject that role for other databases and plaintext connections before broader allow rules.
The [staging isolation guide](../operations/staging-data-isolation.md) describes this first-match
HBA fence. Provision production and staging separately; the staging observation sink is not this
canonical export schema. Activate the role only after this fence and protected credentials are ready:

```sql
GRANT CONNECT ON DATABASE charging_production TO uob_export_runtime;
ALTER ROLE uob_export_runtime LOGIN;
```

Provisioning refuses ambient `PUBLIC` database CREATE/TEMPORARY, non-system schema CREATE,
non-system table/column grants, and publicly executable non-system SECURITY DEFINER routines.
PostgreSQL has no per-role deny that could override such PUBLIC grants. Resolve those grants in
the dedicated destination database before retrying; do not weaken the checks to share a database.
Do not later grant memberships, ownership, unrelated access, or extra privileges to the runtime.
The command checks its current database, not every database in the cluster; the HBA fence remains
mandatory for cross-database isolation.

## Canonical table, identity, and views

`uob.canonical_events` stores a complete serialized `ExportRecord` in `canonical_record jsonb`.
This is the only runtime-writable column. Identity, resource, event type, and timestamp columns
are generated from the record, so a separate identity cannot disagree with the JSON envelope.
The database checks envelope/identity presence and the closed event-kind set; producers must
still validate the complete typed Rust contract before export.

`canonical_events_identity_key` is a `UNIQUE NULLS NOT DISTINCT` constraint over
`(environment, bridge_id, record_id, subrecord_id)`. Missing and explicit-null child IDs represent
the same root; ordinal zero is a distinct child. Environment and bridge remain part of identity.
Retries can use `ON CONFLICT ON CONSTRAINT canonical_events_identity_key DO NOTHING`, followed
by SELECT of the canonical record. The provider must compare the complete JSONB content:
identical content is a duplicate; different content is an integrity error, never an overwrite.
The schema alone does not classify that result or provide the future adapter's atomic batch logic.

Indexes cover this identity, resource JSONB containment (GIN), source/observation timestamp text,
and `event_type`. Timestamp text retains the original UTC fractional precision, including absent
source time; it is not converted to PostgreSQL's microsecond precision. Text indexes support exact
timestamp lookup, not chronological ordering across different fractional widths. Analytical queries
needing chronological ranges must explicitly cast to `timestamptz`, accepting its precision limit;
the original text and canonical record remain available.

All three views include every canonical table column, including `canonical_record`, plus
`payload`, the unchanged `payload.data` JSONB object:

| View | Event kind | Additional projection |
| --- | --- | --- |
| `uob.measurements` | `measurement` | `value` typed JSONB, `quality` JSONB, `unit` from `measurement.original_unit`, and `measurement_source_time` / `measurement_observed_at` text |
| `uob.transactions` | `transaction_lifecycle` | Complete transaction lifecycle payload, resource, and start/end times remain in `payload` |
| `uob.command_results` | `command_result` | Complete lifecycle result, return route, and separately observed effects remain in `payload` |

Exact decimal strings are never cast to floating point. In measurements, `value` is the canonical
typed value while `unit` describes the **original** measurement; use
`payload #>> '{measurement,original_value}'` with that original unit. Phase, context, location,
quality reason, freshness, and protocol reference remain in `payload`. Point changes and resource
status changes remain queryable in `canonical_events`; they are not misclassified as measurements.
JSONB preserves canonical data, not original JSON whitespace or object-key ordering.

The runtime receives only schema USAGE, table SELECT and column-level INSERT on `canonical_record`,
and SELECT on the views. It cannot update/delete/truncate records, create/alter objects, read the
migration metadata, or access private unrelated tables. The administrator owns every object and
retains administrative recovery authority; append-only is the runtime access policy.

## Upgrades and acceptance checks

The SQL migration inventory is independent of the canonical contract's `schema_version`.
Database version 1 creates the canonical table and administrator-only `schema_version` metadata,
including the bound runtime role. Version 2 adds indexes and preserving views without rewriting
canonical records. Both migrations ship under
[`packaging/postgresql/export`](../../packaging/postgresql/export).
The public command always targets version 2; the test harness uses version 1 to exercise a populated
upgrade. Repeated upgrades are safe and preserve activated LOGIN and CONNECT settings.
Run upgrades during a maintenance window: index creation is transactional, not concurrent.

Upgrades reject future/unknown versions, role substitution, changed ownership, role escalation,
unrelated grants, and unexpected objects. They restore the intended runtime table/column grants,
including removing column-level UPDATE grants that a table-level REVOKE alone would miss.
Back up the destination before administrative changes; no downgrade or destructive reset command
is provided.

Run `./scripts/test-postgresql-provisioning.sh` for the Docker-only acceptance check. It uses the
same pinned PostgreSQL 17 image as the staging checks, with no network, host mounts, or published
ports. It executes the shipped command for fresh and populated upgrades, verifies exact canonical
fixtures and all views, tests duplicate identities and actual runtime permission errors, and checks
unsafe grants, role substitution, quoted identifiers, failed-install rollback, and future versions.
The Rust repository CI workflow runs this check; it does not require a PostgreSQL runtime adapter.
