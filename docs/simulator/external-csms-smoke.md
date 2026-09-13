# Standalone external CSMS smoke

The opt-in `tests/external-csms` harness runs the installed `uob-sim` executable against
[Mobility House OCPP 2.1.0](https://github.com/mobilityhouse/ocpp/tree/2.1.0).
Upstream owns message routing, CALL/CALLRESULT correlation, serialization and schema
validation. `peer.py` supplies a small, explicitly bounded CSMS test policy using the
[documented server interface](https://ocpp.readthedocs.io/en/latest/usage/server_side.html).
This is independent protocol-stack interoperability, not qualification against a complete
production CSMS or proof of OCA certification.

## Why this isolated fallback exists

The selected Rust `ocpp-client` dependency implements the charge-point side; it cannot act
as the independent central system. The Rust `ocppsim` and `ocpp-charge-point-simulator`
candidates recorded in PLAN.md are also charger simulators. Using the bridge's own CSMS
would fail the independence requirement; writing another Rust server with its codecs would
not provide external implementation evidence. No independent dual-version Rust CSMS was
established in that dependency review. The concrete missing capability is an independent
CSMS with both protocol editions and outbound remote start/stop, so this test uses the
external Python stack as a limited fallback.

Python OCPP and its complete dependency closure are version/hash pinned in
`tests/external-csms/requirements.txt`; the Python runtime image is pinned by digest.
They are used only by this manually invoked harness. Nothing adds them to Cargo,
production packages, standard demos, workspace verification or default CI.

## Reproduce using an installed package

On a disposable Linux host with Docker, Bash, GNU coreutils and Python 3, obtain the native
simulator archive from a successful [platform package run](../operations/platform-packages.md).
Verify its checksum and extract into a fresh directory. Pass the extracted executable:

```sh
./scripts/test-external-csms.sh \
  /absolute/path/to/installation/bin/uob-sim \
  /tmp/new-external-csms-evidence
```

The destination must not exist. Build-time internet access installs the hash-verified
external dependencies. The script assembles a temporary Docker context containing only
that binary, the CSMS harness and its two scenario files. Each runtime container has no
host mounts or external network, a read-only root, private 32 MiB temporary storage,
256 MiB memory, one CPU, 64 PIDs, no added capabilities and an unprivileged user. There
is no bridge executable, database, management API or Rust source inside it. The peer and
simulator communicate only over the container's loopback WebSocket endpoint.

Each exchange has a five-second deadline, each simulator run has a 45-second deadline,
and Docker executions have outer 100/120-second deadlines. The image build is a separate
setup operation. Containers, temporary build inputs and the task image are removed after
the run. The script preserves evidence in the requested directory.

The simulator receives an ordinary station TOML containing only station identity,
`ws://127.0.0.1:PORT/alpha`, the explicit OCPP version and queue/topology limits. This
isolated plaintext endpoint requires no credentials. Authentication, TLS and production
station admission are outside the exercised subset.

For local debugging in a disposable environment (without container isolation):

```sh
python3 -m venv /tmp/external-csms-venv
/tmp/external-csms-venv/bin/pip install --require-hashes \
  -r tests/external-csms/requirements.txt
/tmp/external-csms-venv/bin/python -B tests/external-csms/run.py \
  --simulator /absolute/path/to/installation/bin/uob-sim \
  --output /tmp/new-local-external-evidence
```

An operator can also run the installed binary directly with a separately supplied public
endpoint configuration and `tests/external-csms/scenarios/1.6.toml` or `2.0.1.toml`, using
`uob-sim run --config CONFIG --scenario SCENARIO --seed 171 --format jsonl`.
Those scenarios deliberately expect this peer's fixed responses, transaction ID 42
(1.6) or `tx-201-42` (2.0.1), tokens and remote commands. A different external endpoint
needs a reviewed scenario for its actual subset; do not remove assertions merely to get a
pass. In particular this test does not establish arbitrary CSMS-assigned transaction-ID
substitution or general external credential interoperability.

## Exercised subset and evidence

| Edition | Charger calls | CSMS calls and observed follow-up |
| --- | --- | --- |
| OCPP 1.6J | BootNotification, Heartbeat, Authorize, StatusNotification, StartTransaction, MeterValues, StopTransaction | RemoteStartTransaction/RemoteStopTransaction accepted separately; start/meter/stop then observed |
| OCPP 2.0.1 | BootNotification, Heartbeat, Authorize, StatusNotification, TransactionEvent Started/Updated/Ended on EVSE 2/connector 1 | RequestStartTransaction/RequestStopTransaction accepted separately; sequenced transaction events then observed |

Success requires both simulator exit 0 with a `run_passed` JSONL summary and the exact
external action-count set, including accepted remote responses. A missing, duplicated,
rejected or unsupported exchange cannot count as success. Unhandled peer failures,
timeouts, malformed output and dependency drift fail the run.

`smoke.json` and `mismatch.json` identify every external dependency, Python/platform,
simulator SHA-256, harness/scenario hashes, seed, observed actions, exit status and full
simulator JSONL events. `image-id.txt` identifies the assembled runtime image. Deliberate
boot rejection is tested for both editions; raw mismatch evidence remains `failed`, with
simulator exit 3 and `unexpected_protocol_response`. The wrapper passes only when normal
smokes succeed **and** these negative controls fail exactly as expected. Runner
`--inject-mismatch` itself always returns nonzero for that rejection.

No persistence, reconnect, TLS, security/certificate operations, smart charging, full
feature coverage, ARM/Pi performance or production-readiness claim follows from this smoke.

## Recorded package rehearsal

The [2026-09-13 x86-64 evidence](../../tests/external-csms/evidence/2026-09-13-x86_64.json)
records the unmodified `uob-sim` 0.28.0 archive from successful package run
[34763607556](https://github.com/MSpiechowicz/universal-ocpp-bridge/actions/runs/34763607556),
source `c47db330955cd43c73a84b50773f5d28a15c5984`, built with Rust 1.98.0.
Both protocol smokes passed in the container; both rejected-boot controls failed with
exit 3. The archive checksum and executable digest were verified against the package
checksum and manifest. This dated evidence qualifies only the recorded subset and inputs.
