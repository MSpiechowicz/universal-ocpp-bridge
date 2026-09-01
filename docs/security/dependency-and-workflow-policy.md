# Dependency and workflow security policy

The security workflow makes automated evidence available for review; it does not replace review or
establish that the bridge is secure. It runs on pull requests, `main`, a weekly schedule, and manual
dispatch. Jobs use disposable GitHub-hosted runners, read-only repository permissions, bounded
timeouts, and no production, signing, or publishing credentials.

## Rust dependencies

[`deny.toml`](../../deny.toml) fails on RustSec vulnerabilities, unsound dependencies, unknown
registries, and every Git source. The license allowlist is the reviewed set currently required by
`Cargo.lock`; a new license or source requires an explicit policy change in the same review as the
dependency. Unmaintained packages are denied when they are direct workspace dependencies.

One reviewed advisory exception is recorded for RUSTSEC-2026-0009. The vulnerable `time` path is
RFC 2822 parsing; this repository parses timestamps only with `Rfc3339`. The patched `time` release
requires Rust 1.88 while the application toolchain is pinned to Rust 1.87. Remove the exception and
upgrade to `time` 0.3.47 or newer when the workspace MSRV moves to 1.88. Any future exception must
likewise name the advisory, demonstrate that the vulnerable path is unreachable, and state a
removal condition.

The committed lockfile is authoritative for checks and SBOM generation. Dependency updates arrive
as reviewable weekly Dependabot pull requests. They run the same checks as any other change and are
never auto-merged or used to silently downgrade a security-sensitive dependency.

## Secrets and workflows

Gitleaks scans full Git history with findings redacted. It cannot comment or upload its own report,
so its read-only token cannot become a write path. The checked-in fake marker is recognized only by
the fixture configuration and proves that a matching secret fails the job without placing a
credential-shaped value in history.

Zizmor audits only real workflows in its normal step. A separate deliberately insecure fixture
proves that unpinned actions, broad permissions, and execution of pull-request code in a privileged
trigger fail the audit. Every real third-party action is pinned to a full commit SHA. Updating the
human-readable release comment alone is insufficient: review the upstream release and change the
SHA and comment together.

Untrusted pull-request jobs have no production, signing, deployment, or publishing credential and
do not use privileged writable caches. Future privileged release jobs must use separate protected
environments and cache namespaces and must never execute artifacts produced by untrusted jobs.

## SBOM evidence

The workflow generates a CycloneDX JSON source SBOM with Syft 1.51.1. Its primary component names
the repository and exact source commit, and a validation step requires every registry dependency
and version from `Cargo.lock` to appear. The source SBOM is retained for 14 days. Once installable
packages exist, package workflows must generate and retain a separate SBOM for each immutable
artifact digest; a source SBOM is not package evidence.

Run the existing workspace checks locally with `./scripts/verify-workspace.sh`. The external
security scanners run in GitHub Actions because their advisory databases and release binaries
require network access. Each downloaded release archive has a reviewed SHA-256 digest in the
workflow; the download is rejected before extraction if its bytes change.
