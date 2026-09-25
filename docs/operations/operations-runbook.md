# Production installation and operations runbook

This is a chronological **production-only** installation on a clean Linux
systemd host. It installs the non-root `uob.service` and the separately built,
independently running release supervisor. It does **not** install staging,
sign a release, activate an artifact or qualify charging on a physical host.
The packaged production binary starts a management/storage service; charging
workflows are not composed in this profile. `READY=1` and a live management
socket do not prove charging readiness; `/health` can report HTTP 503 for
starting core/storage. Do not attach production chargers on this evidence.
See [service lifecycle](service-lifecycle.md), [platform packages](platform-packages.md)
and [health semantics](health-readiness-metrics.md).

Run installation commands below as the host administrator from a reviewed
checkout containing `packaging/`. Choose a Linux systemd 245+ host; verify
executable architecture, OS/glibc baseline and distribution policy before
provisioning. Use an administrator-reviewed trusted distribution of **two
separate binaries**: `uob` and `uob-release-manager`. The CI `uob` tarball
contains no supervisor and its same-run SHA-256 detects corruption, **not**
a trusted signature; it is not a signed artifact-store bundle. Verify
provenance by the site's distribution process. Do not extract an unsigned
CI archive into `/var/lib/uob-releases` or treat it as promotable.

## 1. Prepare the host and production identity

Select a unique production bridge ID; plan a durable production-state filesystem,
journal/WAL capacity and protected backup retention before starting. On a new
installation, provision fixed ext4 mounts for `/var/lib/uob` and
`/var/lib/uob-releases` with independent capacity; if co-hosting optional staging,
also provision `/var/lib/uob-staging` on its **own** fixed partition, offline on an
empty deployment disk. Do not repartition an active host. Production boot has no
staging dependency. The administrator-controlled release mount needs the reserves
required by [disk admission](staging-disk-preflight.md) and
[signed installation](signed-artifact-store.md). This procedure never deletes or
moves an existing database, WAL, marker, artifact or reservation to make space.

Before installing, inspect the two actual mounts, their ext4 filesystem types,
distinct fixed backing devices, capacity and inode headroom. If either command
fails or points to an unexpected mount, **stop**; never let systemd create
production state on the root filesystem as an unnoticed fallback. Substitute
only previously verified binary input paths below:

```sh
findmnt --mountpoint /var/lib/uob
findmnt --mountpoint /var/lib/uob-releases
install -m 0755 -o root -g root /path/to/verified/bin/uob /usr/local/bin/uob
install -d -m 0755 -o root -g root /usr/local/libexec
install -m 0755 -o root -g root /path/to/separately/verified/uob-release-manager /usr/local/libexec/uob-release-manager
/usr/local/libexec/uob-release-manager --version
install -m 0644 packaging/systemd/uob-sysusers.conf /etc/sysusers.d/uob.conf
systemd-sysusers /etc/sysusers.d/uob.conf
install -d -m 0750 -o root -g uob /etc/uob
install -m 0640 -o root -g uob packaging/systemd/bridge.toml /etc/uob/bridge.toml
```

On an existing installation, **do not run the sample-config install command**:
preserve its unique ID, root-owned configuration, secrets, identity marker and
database. Edit the clean-host `/etc/uob/bridge.toml` to replace `site-01`;
keep `environment = "production"` and loopback-only management. Keep secrets
root-owned/service-group-readable and out of arguments, configuration values
and logs. Validate offline before enabling the unit:

```sh
/usr/local/bin/uob config check --config /etc/uob/bridge.toml
```

Production station transport requires TLS and a per-station high-entropy secret;
mutual TLS additionally requires a client CA and exact station certificate
binding. Provision/rotate scoped management `read`, `control` and
`privileged_control` grants separately; loopback is not command authorization.
The packaged listener is loopback-only and does **not** compose remote TLS
management. Do not publish it via an ad-hoc proxy or enable a plaintext
production charger port. See [station transport](../security/station-transport-authentication.md),
[management scopes](../security/management-and-integration-access.md),
[local authorization](../security/local-authorization.md) and
[headless CLI limitations](headless-cli.md).

## 2. Install the production unit

```sh
install -m 0644 packaging/systemd/uob.service /etc/systemd/system/uob.service
install -m 0644 packaging/systemd/uob-production.slice /etc/systemd/system/uob-production.slice
install -m 0644 packaging/systemd/journald-uob.conf /etc/systemd/journald@uob.conf
systemd-analyze verify /etc/systemd/system/uob.service
systemctl daemon-reload
systemctl enable --now uob.service
systemctl show uob.service -p User -p Group -p MainPID -p Result -p NRestarts
ps -o user,group,pid,args -p "$(systemctl show uob.service -p MainPID --value)"
journalctl --namespace=uob -u uob.service
curl --max-time 3 -i http://127.0.0.1:8080/health
curl --max-time 3 http://127.0.0.1:8080/metrics
```

Check that the PID belongs to `uob`, the unit is in `uob-production.slice`
with `MemoryMax=256M`, and state/runtime directories are private (0700).
If startup fails, check `systemctl status uob.service` and the namespaced
journal; do not change state-file permissions, remove locks or delete WAL.
The current management/storage health can be 503; a responding endpoint is
not a charging acceptance test. `Type=notify`, a 30-second startup limit and
a 10-second watchdog cover that composed loop, not charger completion.
The application store's logical seven-day journal/outbox policy reserves
16 MiB of a default 256 MiB budget for active-session completion and refuses
new starts before that reserve is used **where charging is composed**;
the shipped production unit has no charging ingress. Physical ENOSPC is
separate. Observe `readiness` versus `accepts_new_sessions`, storage
pressure, queue/reconnect counters, RSS/CPU and local response latency
through `/health` and `/metrics`. Target, broker, EMS and external-export
outages are component degradation, not by themselves local core failure.
No Pi CPU/RSS/latency or co-host capacity target is measured by these steps.
See [storage admission](../architecture/storage-retention-admission.md),
[watchdog evidence](service-watchdog.md) and
[staging controls](staging-resource-governor.md).

The `uob` journal namespace bounds retention (32 MiB persistent, 8 MiB
volatile, seven days), with rotation overhead; it is not a partition quota.
Inspect per-invocation `uob_service_exit` and component evidence before
recovery. Query the unit's `Result`, `ExecMainCode`, `ExecMainStatus` and
`NRestarts` via `systemctl show uob.service`. Normal SIGTERM drains requests
and SQLite within `[lifecycle].shutdown_timeout_seconds` (default 20 seconds);
`TimeoutStopSec=25s` and cgroup killing bound a stuck process. If increasing
that setting, adjust the unit timeout via a reviewed drop-in. Preserve
committed state and WAL after abnormal termination.

## 3. Install independent release status

The manager is **not** supplied in the unsigned `uob` CI archive or in signed
application bundles. Its unit has no dependency on `uob.service`. Provision an
owner-controlled, regular, non-hardlinked
`/etc/uob-release-manager/install-policy.json` with the actual host `architecture`,
`os_id`, numeric `os_version`, independently inventoried `current_formats`, trusted
Ed25519 public keys (never demo keys), security floor/revocations, and positive
backup/previous reserves. Use the [InstallPolicy fields](signed-artifact-store.md#administrative-trust-and-disk-setup),
not candidate claims, to fill it. An unversioned or mismatched OS prevents supervisor
startup. Mount and provision the canonical root-owned `/var/lib/uob-releases` store
according to the signed-store guide **before** starting the manager.

```sh
findmnt --mountpoint /var/lib/uob-releases
install -d -m 0755 -o root -g root /var/lib/uob-releases
install -m 0644 packaging/systemd/uob-release-manager-sysusers.conf /etc/sysusers.d/uob-release-manager.conf
systemd-sysusers /etc/sysusers.d/uob-release-manager.conf
install -d -m 0700 -o root -g root /etc/uob-release-manager
# Provision the site-reviewed root-owned, mode-0600 install-policy.json first.
install -m 0600 -o root -g root packaging/systemd/release-supervisor.json /etc/uob-release-manager/supervisor.json
install -m 0644 packaging/systemd/uob-release-manager.service /etc/systemd/system/uob-release-manager.service
systemd-analyze verify /etc/systemd/system/uob-release-manager.service
systemctl daemon-reload
systemctl enable --now uob-release-manager.service
systemctl show uob-release-manager.service -p ActiveState -p Result -p MainPID
/usr/local/bin/uob release status --format json
systemctl stop uob.service
systemctl show uob.service -p ActiveState -p MainPID
systemctl show uob-release-manager.service -p ActiveState -p MainPID
/usr/local/bin/uob release status --format json
/usr/local/bin/uob release events --format jsonl
```

The policy provision comment is a prerequisite, not a generated policy:
inspect owner/mode of policy and configuration before starting. Run status
as root (the shipped grant maps UID 0 to `read`, `stage`, `activate`).
For less privileged operators, add only intended UID-specific permissions
to the protected JSON and group membership for socket access, then restart
the manager. Group access grants **no** IPC operation. `status`/`events`
need `read`, stage/qualify need `stage`, promote/rollback need `activate`;
never add staging or broad users to this socket group. Expect `protocol`,
`manager_version` and `code` from status **after the bridge is stopped**;
stop has no `Requires`/`BindsTo` effect on the manager. A fresh store has
no previous-good or qualification. A failed manager startup or `forbidden`
response is not reason to weaken ownership, permissions or policy: inspect
`journalctl -u uob-release-manager.service` and the operator UID/grants.
Re-enable production only after reviewing its startup and data state:
`systemctl start uob.service`.

## 4. Optional release and staging decisions

For signed delivery, use only separately signed format-1 `manifest.json`,
`manifest.sig`, `payload.bin` with pinned administrator trust. Only **after** the
site has supplied these real artifacts, approved trust policy, separately signed
qualification evidence and an independently healthy previous-good installation,
the operator can use the following CLI sequence (replace `DIGEST` with the exact
installed 64-character lowercase SHA-256 digest):

```sh
/usr/local/libexec/uob-release-manager install /var/lib/uob-releases /etc/uob-release-manager/install-policy.json /srv/delivery/candidate/manifest.json /srv/delivery/candidate/manifest.sig /srv/delivery/candidate/payload.bin
/usr/local/libexec/uob-release-manager verify /var/lib/uob-releases /etc/uob-release-manager/install-policy.json DIGEST
/usr/local/bin/uob release stage --bundle /srv/delivery/candidate
/usr/local/bin/uob release qualify --release DIGEST --evidence /srv/delivery/evidence.json
/usr/local/bin/uob release status --format json
/usr/local/bin/uob release promote --release DIGEST
```

`install` verifies an immutable candidate, not production activation. `stage`
**reverifies an already installed candidate**; it does not upload it or run staging.
The exact signed evidence and its detached signature must be independently published
to the protected evidence inbox **before** `qualify`; the CLI passes its digest only.
Qualification needs a trusted harness, actual old→new→old compatibility, required
suites and a continuous 24-hour soak. Successful preflight through the standalone
IPC returns `activation_blocked`, not a switch. If previous-good is missing
(including first installation), revoked, below a security floor, unsupported for
current data, or evidence is missing, do not force promotion or fallback. The
[preflight policy](release-preflight-backup.md) is a separately provisioned optional
read-only production access profile, not enabled by the default manager unit.

Staging is **optional** for production boot, but actual candidate qualification
requires independently run isolated staging/acceptance evidence. If later
authorized, install [separate users/filesystems](environment-filesystem-isolation.md),
the [loopback-only test network](environment-network-isolation.md), mandatory
[disk admission](staging-disk-preflight.md),
[staging governor](staging-resource-governor.md) and
[test data/peer isolation](staging-data-isolation.md) before starting peers.
All simulator, broker, database and provider test peers belong in that governed
staging slice/network with synthetic identities and separate credentials; broker
ACLs must deny production topic/discovery and command access. A **shared broker
is not an available mode** in the shipped staging namespace. The optional
PostgreSQL staging role/database are distinct and denied production access by
privilege and ordered HBA rules; do not bypass the namespace with a host proxy
to reach a shared production server. A sanitized import is status-only and
reidentified, never production SQLite/WAL, transactions or secrets.
A separate Linux test host still needs its own staging limits and isolated
test peers. The co-hosted governor requires a healthy production `/health`;
the current management-only production profile reports 503 for unstarted
charging core/storage, so do not weaken this admission gate to force co-host
staging. Prefer the isolated test host until production health is actually
qualified. Do not start peers outside the slice.

See [recovery](release-recovery-runbook.md) for incident decisions and
[disposable rehearsal](runbook-rehearsal.md) for the current testable evidence
boundary. This documentation does not attest a privileged live systemd
installation, live promotion/rollback, or Pi qualification.
