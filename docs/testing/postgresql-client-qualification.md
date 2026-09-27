# PostgreSQL client qualification (#103)

This is evidence for the **private client** in `adapters/export-postgresql`, not an
export-provider acceptance result. The service still advertises PostgreSQL as unavailable and
rejects enabled PostgreSQL export. There is no PostgreSQL `DatabaseProvider`, export scheduler,
or end-to-end export claim. The [provider conformance suite](database-provider-conformance.md)
remains a separate acceptance gate for that future integration.

## Reproduce on a disposable x64 Linux host

The reviewed #103 checkout pins `tokio-postgres = 0.7.18` and
`postgres_rustls = 0.1.6` directly in `Cargo.toml`, with resolved packages in `Cargo.lock`.
[`scripts/test-postgresql-client.sh`](../../scripts/test-postgresql-client.sh) pins PostgreSQL 17
to `postgres:17@sha256:e42539a54ee3e82e21f19d7eded61b579869585f86b90b5e851ff0d3dd8b4001`.
Once the reviewed branch is available remotely, a fresh checkout can run:

```sh
git clone --branch feat/103-postgresql-client-qualification https://github.com/MSpiechowicz/universal-ocpp-bridge.git
cd universal-ocpp-bridge
./scripts/test-postgresql-client.sh
```

After merging, run the same script from a fresh checkout of the containing revision instead.
Requires a Linux host with `/proc`, Docker daemon access, OpenSSL, Python 3, Cargo and the Rust
1.98 toolchain. The script builds the client with `cargo build --locked --release -p
uob-postgresql-export-adapter`, starts a disposable Docker PostgreSQL instance with an ephemeral
127.0.0.1 port and random credentials, generates short-lived local certificates, runs a loopback
fault proxy, and removes its temporary files/container on exit. It does not use an existing
production or staging database. Docker daemon access is privileged; use a trusted disposable
runner, not a shared production host. The test image needs to be available or pullable; the
loopback-published port is local to the test host, not proof of network isolation from other
local processes.

The Rust workflow runs `./scripts/test-postgresql-client.sh` after workspace, staging and
provisioning checks on `ubuntu-24.04`, with `contents: read`, checkout credentials not persisted,
a 20-minute step limit and a 45-minute workspace-job limit. These limits cap CI execution; the
CI run itself has **not** been observed to pass for this change. The release job remains gated on
the workspace job. Separately, `./scripts/verify-workspace.sh` passed locally (formatting,
Clippy, workspace tests and boundaries), followed by the live client run below.

## Observed x64 Linux result

A full live `./scripts/test-postgresql-client.sh` run passed on x64 Linux against the pinned
PostgreSQL 17 image with AWS-LC TLS. The client reported `driver_tasks=0` after scenarios. Its
security cases covered a successful verified TLS connection to the fixture configured for SCRAM;
rejection of untrusted CA, malformed CA, wrong hostname, expired and not-yet-valid server
certificates; rejection of production and staging plaintext; and permitted Demo plaintext to a
loopback endpoint. The fixture verifies its disposable role and HBA rule use SCRAM-SHA-256; this
does **not** mean the client enforces SCRAM as the server-selected authentication mechanism.
Production and staging require verified TLS regardless of authentication mechanism. Demo plaintext
to a non-loopback endpoint was rejected as `Permanent` (`postgres.transport.invalid`) before
reading the credential file or opening a socket. Protected credential input checks rejected
permissive file mode, symlink, missing file, directory, FIFO, oversized file and invalid TOML
without disclosing the test passwords in adapter output. A TLS refusal was permanent; connection,
TLS and authentication stalls were retryable.

The observed connection/TLS/authentication stall deadline was 5.001 s, the slow-query deadline
1.530 s, and cancellation 183 ms. The client uses a 5 s connection deadline, a 1.5 s query
limit and a 150 ms cancellation trigger; these observations are on one x64 host, not hard
real-time guarantees. A normal committed transaction left exactly one marker. A dropped
pre-commit response returned `Uncertain`, with zero persisted markers after rollback; a suppressed
commit acknowledgement returned `Uncertain`, with exactly one committed marker. A lost response
after INSERT also returned `Uncertain`, not an invented success or permanent failure.

The process-level stress measurements from that run were:

| Workload | Iterations | Baseline / peak / final RSS (KiB) | Elapsed | Other observed result |
| --- | ---: | ---: | ---: | --- |
| Successful connections | 100 | 6780 / 6868 / 6868 | 588 ms | Completed |
| Failed connections followed by recovery | 1000 | 6472 / 6564 / 6564 | 293576 ms | Completed |
| Canceled slow queries | 100 | 6784 / 6876 / 6876 | 15655 ms | Completed |
| Pre-commit rollbacks | 100 | 6484 / 6560 / 6560 | 33470 ms | No rollback marker persisted |

The final file-descriptor count was 9, peak concurrent client sockets 1, and driver tasks 0.
The stress probe enforces peak RSS at most 65536 KiB, final RSS at most baseline + 8192 KiB,
final file descriptors at most 16, peak sockets between 1 and 2, and no remaining client socket
or driver task after each iteration. These bounds are fixture gates, not a capacity forecast for
a running service. This was x64 evidence only: no Raspberry Pi run, thermal/CPU/I/O qualification,
or Pi resource bound has been measured.

A later, separate full live run of the same script classified a startup TCP reset as `Retryable`
(`postgres.connect.unavailable`, `elapsed_ms=28`, `driver_tasks=0`). The following trusted,
verified connection succeeded (`outcome=ok`, `elapsed_ms=33`). Invalid CA, wrong hostname and
invalid server certificate cases remained `Permanent`. That run also completed 1000
failed-connection recoveries, 100 cancellations and 100 pre-commit rollbacks with peak concurrent
client sockets 1 and `driver_tasks=0`. These supplemental observations do not replace the
earlier run's stress measurements above; neither a CI pass nor Raspberry Pi qualification has
been observed.

Another separate full live run of the script classified a startup TCP reset as `Retryable`
(`elapsed_ms=29`, `driver_tasks=0`), followed by a successful trusted, verified connection
(`elapsed_ms=32`). The loopback fault proxy then sent an orderly TLS `close_notify` after receiving
the startup packet but before authentication; the client classified that clean close as `Retryable`
(`elapsed_ms=68`, `driver_tasks=0`). The next trusted, verified connection succeeded
(`elapsed_ms=31`, `driver_tasks=0`). Invalid CA, wrong hostname and invalid server certificate
cases remained `Permanent`. This run also completed 100 successful connections, 1000
failed-connection recoveries, 100 cancellations and 100 pre-commit rollbacks, with peak concurrent
client sockets 1, final file descriptors 9 and `driver_tasks=0`. The reset and clean close were
scripted client-fixture faults, not observed failures of the disposable database or evidence of
production behavior. This run does not replace the earlier stress table or extend its x64, CI
and Raspberry Pi limitations.

A later, separate post-hardening full live run exercised the backend response guard over trusted
TLS. The guard limits each backend frame (including its five-byte header) to 1 MiB and cumulative
backend wire bytes between `ReadyForQuery` messages, including startup, to 2 MiB; it also wraps
plaintext connections. A hostile fixture sent only the five-byte header advertising a
2,147,483,647-byte frame, then in a separate scenario sent 65 smaller `CommandComplete` frames
without `ReadyForQuery` (2,129,920 bytes total). The client promptly rejected each with sanitized
`postgres.connect.setup` and `retry=Retryable` (117 ms and 116 ms respectively), reported
`driver_tasks=0`, and closed the peer socket. A subsequent trusted, verified probe succeeded
after each rejection. The same run completed 1000 failed-connection recoveries, 100 cancellations
and 100 pre-commit rollbacks with peak concurrent client sockets 1, final file descriptors 9 and
low RSS. These fixture observations are distinct from the earlier stress table; they do not
establish CI, Raspberry Pi, provider integration or production workload safety.

Rustls uses the workspace's existing `aws_lc_rs` provider, avoiding a workspace-wide provider
collision with `ring`. `cargo tree --locked -e features -i rustls --depth 1` showed no Rustls
`ring` feature.

## Upgrading the client or test database

Treat the two direct Rust pins, `Cargo.lock`, and the PostgreSQL image digest as a reviewed set.
For a proposed upgrade, inspect upstream release notes and security advisories, update the exact
pins and lockfile and/or reviewed PostgreSQL 17 image digest, then rerun from the resulting
checkout:

```sh
./scripts/verify-workspace.sh
./scripts/test-postgresql-client.sh
./scripts/test-postgresql-provisioning.sh
```

Capture the new version/digest, full pass/failure output, fault classifications, deadline and
stress measurements; compare them with the bounds above before replacing this evidence. Recheck
TLS trust/hostname/time validation, plaintext policy, credential protections, transaction
uncertainty and task/socket cleanup rather than accepting a successful probe alone. A future
provider/scheduler must separately pass provider conformance and real export integration checks;
qualify on the intended Raspberry Pi hardware before making Pi performance or reliability
claims. A dependency bump, an image change, or a green CI gate does not by itself enable export.
