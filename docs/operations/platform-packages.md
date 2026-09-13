# Native Linux runtime packages

The platform package workflow builds Linux x86-64 and ARM64 independently on native
`ubuntu-24.04` and `ubuntu-24.04-arm` runners. Both use the reviewed Rust 1.98.0
Bookworm container digest, locked Cargo dependencies and committed browser assets.
The existing frontend workflow checks those embedded assets against frontend source.

`./scripts/build-platform-packages.sh` produces separate versioned `uob` and `uob-sim`
tarballs under `target/platform-packages`. Docker, Python 3.11+ and Git are builder
requirements only. Packages require Linux with glibc 2.36 or newer. The architecture
must match the execution host. Cross-compilation and emulation are not used in this
qualification pipeline. A nonmatching host or ELF architecture fails closed.

Each archive contains exactly its executable, this installation guide, the repository
license and a JSON manifest. The daemon embeds the compiled browser console. Its
archive contains no simulator, release manager, provider executable, development
runtime, compiler, scenario fixture, configuration or test credential. The allowlist
rejects extra paths, links, duplicate entries, altered reviewed prose, wrong file
modes, architecture mismatches and content checksum mismatches. This is a packaging
boundary audit; source secret scanning and dependency-boundary checks remain required.

## Install and run

Obtain the architecture-specific archive and its checksum from the same successful
platform CI run. Verify with `sha256sum --check ARCHIVE.tar.gz.sha256`, then extract
into a new version-specific directory with `tar -xzf ARCHIVE.tar.gz -C DIRECTORY`.
These CI checksums detect corruption; they are not a trusted signature or release
promotion authorization. Use reviewed source and distribution until signed candidate
publication is available. Never overwrite the active installation in place.

Install only `bin/uob` on a charging host. Supply a site-owned configuration and run
`bin/uob config check --config /etc/uob/bridge.toml` before starting
`bin/uob serve --config /etc/uob/bridge.toml`. Configure the non-root service account,
filesystem isolation and systemd supervision separately using the repository's
service lifecycle and environment isolation guides. No build tools are needed on
the device. Automatic activation requires the separately signed compatibility and
qualification contracts; these tarballs are not activation-store bundles.

Install the simulator archive in a separate directory on a test host. Run
`bin/uob-sim run --config simulator.toml --scenario scenario.toml --seed 42 --format jsonl`
with your own isolated peer configuration and versioned scenario. The simulator
communicates over WebSockets and can run without a daemon installation. Installing
or starting the daemon never installs or starts the simulator.

## Evidence and limits

`python3 -B scripts/smoke_platform_packages.py --packages target/platform-packages
--output target/platform-packages/smoke.json` audits and extracts the archives into
separate temporary installations. It executes offline daemon configuration validation,
startup, health and identity reads, embedded HTML/JavaScript delivery, graceful shutdown,
and independent simulator Heartbeat exchanges for OCPP 1.6 and 2.0.1. All waits and
outputs are bounded. The current management-only CLI has no composed charging runtime:
its health endpoint must report HTTP 503 with `not_ready` and starting core/storage.
The report records this state explicitly; a smoke pass does not mean charging readiness.
Test peer code and temporary scenario files stay outside archives.

Manifests record source revision/tree, dirty tracked-source state, product version,
actual compiler identity, native target, runtime baseline and per-file SHA-256 hashes.
The smoke report binds results to archive digests and records machine/kernel/OS,
runner name/architecture/image and CI run identity. Failed runs retain a failure
report rather than a synthetic pass. Archives are uploaded only after smoke success.
They are temporary CI validation artifacts, retained for seven days, not immutable
signed release candidates or a supported rollback archive.

**Raspberry Pi performance remains unqualified.** Native ARM64 CI proves package
execution only. No CPU, memory, latency, thermal, storage or co-hosted staging budget
is established here. Physical Pi 4/5 measurements remain outstanding in issue #176.
