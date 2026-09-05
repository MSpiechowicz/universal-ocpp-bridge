# Production and staging filesystem isolation

Production keeps the existing `uob.service`, `uob:uob` account, and paths. Optional staging
uses `uob-staging.service` and `uob-staging:uob-staging`. Each belongs to its own systemd slice.
Only production has a boot install target; neither service depends on the other's unit,
configuration, directories, or successful startup. Staging is started explicitly.

| Resource | Production | Staging |
|---|---|---|
| Configuration and secrets | `/etc/uob/` | `/etc/uob-staging/` |
| Authoritative SQLite | `/var/lib/uob/operational.sqlite3` | `/var/lib/uob-staging/operational.sqlite3` |
| WAL / shared memory | Adjacent `-wal` / `-shm` files | Adjacent `-wal` / `-shm` files |
| Durable identity / ownership lock | `/var/lib/uob/identity.json`, `service.lock` | `/var/lib/uob-staging/identity.json`, `service.lock` |
| Runtime sockets / process lock | `/run/uob/` | `/run/uob-staging/` |
| Reserved optional export spool | `/var/lib/uob/export-spool/` | `/var/lib/uob-staging/export-spool/` |
| Journal namespace | `uob` | `uob-staging` |
| Slice | `uob-production.slice` | `uob-staging.slice` |
| Default management endpoint | `127.0.0.1:8080` | `127.0.0.1:18080` |

systemd creates state and runtime directories with mode 0700 under separate users. A 0077
umask protects new files. Configuration/secrets are root-owned, service-group-readable, and
not service-writable. `ProtectSystem=strict`, empty capabilities, and `NoNewPrivileges=yes`
prevent services from changing installed configuration, executable files, or privileges.
Each unit also makes the other environment's configuration, state, and runtime paths
inaccessible; the `-` prefix permits the peer environment to be absent. Filesystem permissions
also deny cross-user access outside the unit namespace, including pathname Unix sockets.
Do not put either user in the other's group, grant cross-user ACLs, or share writable mounts.

The unit pins `UOB_DEPLOYMENT_ENVIRONMENT`; `uob config check` and `uob serve` reject a
conflicting `[bridge].environment`. systemd supplies `STATE_DIRECTORY` and `RUNTIME_DIRECTORY`.
Packaged startup requires absolute, separate, canonical, mode-0700 directories. It rejects
symlinked directories, symlinked/hardlinked database sidecars and ownership files, duplicate
state/runtime ownership, and reuse of a directory bound to another bridge or environment.
An exclusive state lock protects against accidental use of a different runtime directory.
A durable identity marker survives restarts; a partially written marker fails closed.

`config check` stays read-only and does not require the directories to exist. `serve` acquires
both locks, binds the durable identity, and opens the existing SQLite adapter before exposing
management. Storage drains within the same shutdown deadline as management. It preserves
committed state and WAL on failure; lock files are never deleted to bypass an active owner.
An existing `operational.sqlite3` without an identity marker is rejected for operator review.
Do not delete the marker to relabel a deployment or copy production data into staging.

The current executable remains management-only: it initializes its operational store but
charging handlers and management query ports are not yet composed with that store. This
packaging does not claim those runtime paths are complete. Export remains disabled/unavailable;
the spool path is reserved, and no export worker is created. Future composition must use this
same authoritative store and place optional export state beneath the environment's state root.
Normal development invocations without `UOB_DEPLOYMENT_ENVIRONMENT` retain their existing behavior.

## Installation and layouts

On a production Linux host, follow [service installation](service-lifecycle.md), also installing
`packaging/systemd/uob-production.slice` into `/etc/systemd/system/`. Preserve existing unique
bridge identity and credentials; do not overwrite an installation with the sample configuration.
Use a verified, root-owned executable at `/usr/local/bin/uob`.

For optional staging on the same Pi, run these commands as an administrator after reviewing
available capacity and preparing synthetic test peers:

```sh
install -m 0644 packaging/systemd/uob-staging-sysusers.conf /etc/sysusers.d/uob-staging.conf
systemd-sysusers /etc/sysusers.d/uob-staging.conf
install -d -m 0750 -o root -g uob-staging /etc/uob-staging
install -m 0640 -o root -g uob-staging packaging/systemd/staging.toml /etc/uob-staging/bridge.toml
install -m 0644 packaging/systemd/uob-staging.service /etc/systemd/system/uob-staging.service
install -m 0644 packaging/systemd/uob-staging.slice /etc/systemd/system/uob-staging.slice
install -m 0644 packaging/systemd/journald-uob.conf /etc/systemd/journald@uob-staging.conf
systemd-analyze verify /etc/systemd/system/uob-staging.service
systemctl daemon-reload
systemctl start uob-staging.service
journalctl --namespace=uob-staging -u uob-staging.service
```

The staging sample uses a synthetic bridge identity, a distinct loopback management port,
and no selected target, charger connection, production credential, or export destination.
Do not copy production configuration/secrets. Network peer enforcement and the staging resource
governor are separate backlog items (#145 and #148); the slice here establishes ownership and
accounting, without claiming their firewall, memory, or pressure-shedding guarantees.

For a separate Linux staging host, install only the verified executable and the staging files
above. Use the identical binary digest and configuration schema used for the candidate; no
production account, unit, or database is required. Artifact qualification and candidate activation
remain the release supervisor's responsibility. When testing another artifact on the same Pi,
use a root-owned immutable binary path in a staging-only unit override for **both** `ExecStartPre`
and `ExecStart`; never replace the running production executable to test a candidate.

## Verification

`./scripts/verify-workspace.sh` runs real-process tests for production with staging absent,
failed staging initialization, concurrent production/staging, staging-only deployment, identity
binding, duplicate locks, database creation, restart, and clean shutdown. Unit tests reject
state relabeling, symlinks/hardlinks, and ambiguous/unprotected directories.

`./scripts/test-environment-isolation.sh` runs on a disposable Linux CI runner with Cargo, jq,
systemd tools, and root/sudo available. It verifies the shipped units against the compiled
executable, then runs actual daemon processes as distinct numeric UIDs 60001 and 60002 under
unique temporary directories. Both users attempt to write the peer's database, WAL, identity,
export spool, locks, configuration and secrets, and connect to a peer pathname socket. All
must receive permission denial while own-state writes and independent service boot succeed.
No host accounts or units are installed or started by the test. Do not run it on a charging host.
The normal workspace test run explicitly marks the privileged test/helper ignored; CI runs
the privileged test separately and fails on any missing permission or verification tool.
