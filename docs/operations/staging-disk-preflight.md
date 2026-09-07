# Disk budgets and installation admission

Staging must not compete with the authoritative production journal for disk blocks. The
supported initial layout uses three **dedicated fixed ext4 partitions** mounted at
`/var/lib/uob`, `/var/lib/uob-staging`, and `/var/lib/uob-releases`. Their partition capacities
are the hard environment budgets. Provision these offline on an empty deployment disk; this
repository does not repartition a live host or move existing databases. Size production for
its journal/WAL and seven-day retention, staging for all test databases and export spools,
and releases for retained artifacts and backups. Keep every staging peer's durable data
under its staging partition. Neither service user may write to the release partition.

The admission helper verifies live kernel device, mount, free-block and inode observations.
A subdirectory or bind mount on a shared device fails. Loop images, virtual/thin devices,
network filesystems and pooled/COW filesystems are unsupported and fail closed. Distinct
filesystem IDs alone do not establish independent physical capacity. The helper does not
claim support for project quotas or infer that an unenforced configuration is a quota.
For a separate staging machine, provision the same three-partition layout; the production
partition may remain empty as the reserved operational budget.

Install the root-owned helper and reviewed policy alongside the staging package:

```sh
install -m 0644 packaging/storage/disk_preflight.py /usr/local/libexec/disk_preflight.py
install -m 0644 packaging/storage/disk-policy.json /etc/uob-staging/disk-policy.json
install -m 0644 packaging/systemd/uob-staging-disk.service /etc/systemd/system/
systemctl daemon-reload
```

Configure the three persistent mounts through the host's reviewed mount configuration.
The mandatory `uob-staging-disk.service` pulls in these mounts and checks them before each
staging start. It does not remain active and reuse an old admission result. Test peers must
use the same `Requires=` and `After=uob-staging-disk.service` dependencies as the staging
service, in addition to the existing network and resource governor dependencies. Root-owned
policy paths must match the service's `RequiresMountsFor` paths if customized. Production
boot has no dependency on this optional staging admission service.

Each partition must retain at least 512 MiB available space and 16 free inodes at admission.
The hard partition boundary means filling staging's database or export spool cannot consume
production's reserved journal blocks after admission. Existing bounded journal namespaces
remain required; this change does not relocate system logs or measure Pi storage performance.

Before writing an installation candidate, an administrator or future release supervisor must
reserve its maximum on-disk size, space for the previous artifact, and the maximum consistent
backup size. Counts must include unpacking/temporary overhead where applicable. For example:

```sh
python3 /usr/local/libexec/disk_preflight.py \
  --policy /etc/uob-staging/disk-policy.json reserve \
  --candidate-bytes 134217728 --previous-bytes 134217728 --backup-bytes 268435456
```

The helper serializes admission and uses `posix_fallocate` to allocate real blocks for three
mode-0600 files under a mode-0700 `install-reservation` directory on the artifact partition.
It then rechecks all headroom and syncs the reservation before returning success. The
previous-artifact allocation is deliberately conservative even if a previous-good artifact
already exists. Existing artifacts, pointers, activation state, backups and unrelated temporary
files are never deleted or modified to fit an installation. Errors report required and
available bytes; increase capacity or review explicitly unreferenced temporary files.

A successful reservation is **capacity only**, not artifact qualification or permission to
activate. The separately planned installer must write within the preallocated files without
truncating away their reservation, enforce the declared bounds, verify the artifact, and
create the backup using the consistent database backup interface. It must recheck admission
immediately before installation/activation and hold its activation lock through that operation.
The release-manager executable still does not implement activation. This helper must not be
used as evidence that a candidate is signed, compatible, backed up, or qualified.

A second or crash-interrupted reservation fails closed. Cancellation is an explicit
administrator/release-supervisor operation after proving the reservation is no longer in use;
there is no automatic stale-file eviction. On an allocation failure this invocation removes
only the allocation files it just created. It never deletes active or previous-good artifacts.

`./scripts/verify-workspace.sh` exercises exact headroom boundaries, inodes, shared devices,
real allocation, allocation failure, headroom changes, protected files, and invalid inputs.
`python3 -B scripts/test-disk-isolation.py --disposable` additionally fills a disposable ext4
staging filesystem to ENOSPC and proves production capacity and retained artifacts survive.
That privileged CI test uses fully allocated loop images in a private mount namespace, with
an explicitly test-only topology observer; deployed admission continues to reject loop disks.
