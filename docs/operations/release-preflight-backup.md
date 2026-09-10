# Production release preflight and backup

An authorized `promote` request first revalidates current signed qualification. It then
runs the exact verified candidate's `bin/uob config check --config PATH --secrets`
with the configured production UID/GID, empty environment, closed stdin, and discarded
stdout/stderr. Paths and limits come exclusively from administrator configuration.
A missing policy, invalid configuration, inaccessible secret reference, failed child,
unproven compatibility, or failed backup returns `preflight_rejected`. No drain,
service stop, artifact switch, migration, or database restore occurs.

A successful preflight currently returns `activation_blocked`: idle/drain admission
and production process control are separate work (#156–#157). The private backup
metadata records successful preflight work; it does not authorize a later activation.
A later promotion must run fresh checks, including qualification and production inputs.

## Administrator configuration

Add `preflight_policy` to the supervisor configuration, naming an administrator-owned
JSON policy file. Create `state_directory/backups` with mode 0700, owned by the manager.
For example (replace numeric service identities and format versions with actual values):

```json
{
  "configuration": "/etc/uob/bridge.toml",
  "operational_database": "/var/lib/uob/operational.sqlite",
  "expected_formats": {
    "public_contract": 1,
    "configuration": 1,
    "operational_sqlite": 5,
    "external_database": 1
  },
  "service_uid": 995,
  "service_gid": 995,
  "maximum_backup_bytes": 268435456,
  "timeout_seconds": 60
}
```

The backup limit must fit the install policy's reserved backup capacity. The timeout
is 1–300 seconds for candidate validation, copying and verification, with at most
64 SQLite pages copied per step. Filesystem I/O still depends on the host kernel;
this is not a hard real-time guarantee for a hung storage device.

The shipped supervisor sandbox intentionally denies production paths. Opt in using
the administrator-installed `uob-release-manager-preflight.conf` drop-in. It grants
read-only production configuration/data access and identity switching for the candidate
checker, while staging remains inaccessible. Verify the production service UID/GID
and configuration/secret permissions before enabling this profile. Candidate checker
execution remains limited to a signature-verified artifact and fixed arguments; IPC
cannot supply executable paths. The existing network restriction remains Unix-only.

## Configuration and compatibility evidence

`--secrets` additionally requires a production configuration. It reads at most 64
references from events and enabled targets/transports. Each reference must be an
absolute canonical regular file without hardlinks, with no world access, nonempty,
and at most 64 KiB. It does not contact an authentication server or claim a credential
is accepted by a remote peer. Ordinary `config check` retains its no-secret-read behavior.
Neither checker logs file contents or secret paths.

Preflight requires the signed old→new→old evidence's resulting public, configuration,
local, and external format versions to match the administrator's expected production
formats. It reevaluates both signed manifests' read/write support, additive migrations,
security floors, and durable continuity assertions. The backup also checks the live
SQLite `user_version` against the install policy's current local version. Remote
compatibility relies on the trusted qualification evidence and administrator-provisioned
format inventory; preflight does not migrate or probe PostgreSQL.

## Backup ownership and recovery

The storage adapter opens the source read-only and pins a read snapshot. SQLite's
online backup API includes committed WAL data while allowing concurrent writers.
The copy uses a private new destination, an explicit byte cap, short copy steps,
zero busy-wait allowance, and deadline-aware integrity verification. A completed
copy is synced before bounded metadata and the directory are synced. Source schemas
are never migrated by backup work.

`backups/production.sqlite` and `backups/metadata.json` form one retained slot.
Metadata binds candidate/previous digests, qualification evidence, production
configuration digest, expected formats, and copied bytes. Partial work or an occupied
slot blocks another backup. An operator must preserve/export the existing disaster
recovery evidence according to installation retention policy and empty the slot
before retrying. The supervisor never overwrites or automatically discards a backup.
This keeps repeated requests from consuming unbounded disk space.

Backups contain operational data and remain private. They are outside artifact pointers,
qualification state, and routine rollback selection. Rollback must continue with the live
SQLite and external databases; it must never restore these backup records automatically.

## Verification

The workspace tests cover committed WAL with concurrent writers, valid recoverable
copies, schema/size/deadline failures, corrupt/missing/symlink sources, refused overwrite,
private secret references, candidate rejection/timeout, unauthorized requests, mismatched
external format evidence, and retained backup behavior across supervisor restart.
Signed executable fixtures exercise the exact offline CLI arguments. No test presents
successful preflight as production activation or silently restores the live database.
