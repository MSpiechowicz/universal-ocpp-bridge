# Signed application artifact store

The independent `uob-release-manager` now provides `install` and `verify` operations.
Installation creates a verified **candidate**, never a running service or a known-good release.
The activation journal, staging qualification and production promotion remain separate work.
The existing [normal promotion gate](release-compatibility.md) still requires real old → new → old
compatibility evidence before an artifact can be activated. An install success is not that evidence.

## Bundle format 1

Distribution consists of three regular files: `manifest.json`, a 64-byte detached Ed25519
`manifest.sig`, and `payload.bin`. The signature covers the **exact manifest bytes**, including
whitespace. JSON is not canonicalized or reserialized for verification. A trust key is never read
from the bundle. The verifier uses pinned [ring 0.17.14](https://docs.rs/ring/0.17.14/ring/signature/index.html).

The manifest is the serialized `BundleManifest` in the release-manager library. It includes:

- `bundle_format: 1`, release ID, full source commit, `architecture` (`aarch64` or `x86_64`),
  distribution `os_id`, and a one-to-three-component numeric `minimum_os_version` array.
- `compatibility`: lowercase SHA-256 `artifact_digest`, signed `release_sequence`, read/write
  schema ranges for public contracts, configuration, operational SQLite and external database,
  and additive local/external migration policies. These reuse the existing compatibility types.
- `files`: an ordered list of `{ "path": "bin/uob", "bytes": 1234 }` entries. Exactly one
  service is required; other entries must be beneath `assets/`. Paths are relative ASCII names
  without empty components, dot/dot-dot components, backslashes, or duplicate names.

The payload is the raw concatenation of those regular files in manifest order, with exactly the
signed byte count for each file. SHA-256 covers the entire payload. There are no archive headers,
compression, links, devices, ownership fields or install scripts. Consequently a bundle cannot
install a release-manager executable, systemd unit, credential or absolute path. Static assets have
no executable permission. CI must sign this format when package publication is implemented;
this change does not create a production signing key or enable publication.

Limits are 64 KiB manifest, 256 files, 240 characters per asset path, and 512 MiB total payload.
Empty files are rejected. Extraction and revalidation use a 64 KiB buffer; entry sizes cannot
cause proportional memory allocation. Partial, excess or changed bytes are rejected before a
candidate pointer can change. Each digest directory is immutable: reinstalling the same digest
is rejected, including attempts to attach different metadata to existing bytes.

## Administrative trust and disk setup

Use an administrator-owned canonical artifact mount, readable but not writable by service users.
Only the independently managed release manager and administrative maintenance may write it.
Apply the dedicated ext4 partition and production/staging separation requirements in
[disk budgets and installation admission](staging-disk-preflight.md) first. The installer does not
provision partitions or replace that deployment preflight. Keep the mount's ancestor directories
under administrative control too. The signing private key belongs to the trusted distribution
system, never to the charging host or application bundle.

Provision an owner-controlled JSON `InstallPolicy` outside the artifact directory. Its fields are:

| Field | Meaning |
|---|---|
| `trusted_ed25519_keys` | One to sixteen public keys, each a JSON array of 32 byte values; no default/test key |
| `security` | `minimum_release_sequence` and `revoked_artifacts` digest array |
| `current_formats` | Numeric `public_contract`, `configuration`, `operational_sqlite`, `external_database` versions |
| `architecture`, `os_id`, `os_version` | Actual host architecture, Linux distribution ID and numeric version array |
| `backup_reserve_bytes`, `previous_reserve_bytes` | Positive bounded capacity for backup and previous-release work |

The CLI checks host fields against its actual architecture and `/etc/os-release` (or the standard
`/usr/lib/os-release` fallback). Unknown/unversioned distributions fail closed. Current schema
versions must be supplied from deployment state, never from the proposed artifact. All schema
surfaces are checked conservatively, including external storage when disabled. Update revocations,
security floors and public keys through the administrator's protected provisioning mechanism;
application bundles cannot modify them. The CLI does not accept remote requests or authenticate
operators through a second control path: local OS access is required.

Example invocations after provisioning the store and real policy:

```text
uob-release-manager install /srv/uob-artifacts /etc/uob-release-manager/policy.json manifest.json manifest.sig payload.bin
uob-release-manager verify /srv/uob-artifacts /etc/uob-release-manager/policy.json DIGEST
```

Both return nonzero on rejection. `verify` authenticates the retained exact manifest again, checks
current host/schema/security policy, and hashes the installed files. Missing/extra files, links,
changed permissions, changed bytes or a newly revoked artifact fail revalidation. It checks disk
headroom too. Before staging or activation, keep the store lock, revalidate, and apply the separate
qualification/compatibility and activation-time disk-reservation gates. The CLI releases its lock
on exit, so a previous CLI success must not be treated as durable activation authorization.

## Installation and retention

The process holds `.disk-admission.lock`, using the same OS flock namespace as the administrative
disk preflight. Capacity includes the incoming files, explicit previous/backup reserves and at least
512 MiB remaining headroom. The installer actually allocates file blocks and temporary reserve
blocks with [rustix fallocate](https://docs.rs/rustix/1.1.4/rustix/fs/fn.fallocate.html), then checks
remaining space/inodes again before streaming. Unsupported allocation or exhaustion fails closed.
Reserves are released after successful verification; later backup/activation work reserves its own
space. Existing preflight reservations are not silently consumed or removed, so their occupied
space remains unavailable to this installer.

Verified files are synchronized, sealed read-only (`bin/uob` executable), moved from private
`.incoming` into `artifacts/DIGEST`, and synchronized before atomically replacing `candidate`.
`active`, `previous-good`, and `candidate` are small owner-controlled regular pointer files
containing exactly the 64-character lowercase digest without a newline. The installer writes only
`candidate`. Production activation must use these same references and lock; do not run a service
from an unreferenced directory. A fresh store has no active or previous-good pointers until the
separate activation owner establishes them.

Retention preserves the union of all three references, even when their digests coincide, and
removes only unreferenced digest directories in this dedicated store. Thus replacing a candidate
cannot delete the active or previous verified known-good release. Invalid/dangling pointers stop
installation and cleanup. Never place unrelated files in `artifacts/`.

A crash before publication cannot select partial data. A leftover `.incoming` or `.candidate-next`
blocks further use: inspect it under the same administrative lock and remove only that unselected
temporary state before retrying. After publication, reopening completes unreferenced-directory
cleanup; referenced active/previous-good/candidate directories remain protected. This is candidate
installation recovery, not the future production activation/power-loss journal.

## Evidence

`cargo test --locked -p uob-release-manager` covers signed installation, immutable collisions,
signature/host/schema/floor/revocation failures, partial/trailing/corrupt payloads, traversal and
supervisor paths, file/directory collisions, links, added assets, altered retained signatures,
lock contention, interrupted extraction, disk denial, and retention across reopen. The existing
promotion-gate suite also runs. No physical Pi performance or production activation is claimed.
