# Trusted candidate qualification

The independent supervisor accepts qualification only from authenticated evidence for
the exact installed candidate and current previous-good production artifact. Publishing
or installing signed application bytes alone is insufficient. Qualification never starts
a process, changes the active pointer, restores a database, or publishes a version.

## Trust and provisioning

Add an optional `qualification_policy` absolute file path to the supervisor configuration.
Omitting it disables qualification. Like the install policy, this file must be an
administrator-owned regular file, without hardlinks or peer write access, at most 64 KiB.
Restart the supervisor to reload policy. Create a canonical administrator-owned `evidence`
directory within its private state directory, normally
`/var/lib/uob-release-manager/evidence`, with mode 0700.

The policy JSON contains:

| Field | Administrator-selected value |
|---|---|
| `authorities` | 1–16 `{ "producer": "harness-id", "ed25519_key": [32 raw public-key bytes] }` entries; producer names and keys must be unique |
| `configuration_digest` | SHA-256 of the exact versioned representative staging configuration |
| `dataset_digest` | SHA-256 of the approved synthetic/sanitized test dataset |
| `soak_profile_digest` | SHA-256 of the representative workload specification |
| `required_suites` | Nonempty list of up to 64 unique acceptance suite identifiers |
| `maximum_evidence_age_seconds` | Maximum age since soak completion, from 1 second through 30 days |

Digests are exactly 64 lowercase hexadecimal characters. Names permit ASCII letters,
digits, dot, underscore and hyphen, up to 128 bytes. Configuration and dataset digests
refer to deployment-approved inputs, never paths or claims chosen by an IPC caller.
Keep configurations free of raw secrets before publishing their evidence references.

Provision qualification keys separately from artifact publishing keys. Only the controlled
qualification harness should hold private keys. It must sign results after running the
required suites, a continuous representative soak, and a real old→new→old compatibility
cycle. Do not expose a signing endpoint that accepts caller-supplied pass flags or timestamps.
Signature verification establishes that the configured harness attested to these results;
it cannot independently prove that a compromised harness ran its tests. Key removal on
policy reload invalidates previously recorded qualification.

The acceptance matrix is explicitly administrator-owned: include every required OCPP,
target-mode, security, persistence and failure-recovery suite for the deployment. An empty
matrix is rejected. This change implements evidence verification, not the still-pending
full acceptance suite execution or hardware qualification infrastructure.

## Signed evidence format

`uob_release_manager::qualification::Evidence` is the versioned JSON producer contract.
Sign the exact UTF-8 bytes with detached Ed25519, without canonicalization or reserialization.
The document includes:

- `format: "uob-qualification-v1"` and the key-bound `producer` identity;
- `candidate_digest` and `source_commit` matching the verified installed manifest;
- `configuration_schema` matching the supervisor's configured format and compatibility cycle;
- exact configuration, dataset and soak-profile digests matching policy;
- `soak_started_unix_seconds`, `soak_finished_unix_seconds` and `soak_passed`;
- `suites`, each with `id`, `passed` and a content-addressed detailed `result_digest`;
- full `compatibility` evidence as `RollbackCycleEvidence`, evaluated by the existing
  [reversible compatibility gate](release-compatibility.md), including both artifact digests,
  supported schemas, preserved durable record classes and in-place database continuity;
- optional `pi_measurements_digest`, referencing a signed harness observation of actual Pi
  measurements. Null means no Pi performance evidence; generic ARM64 success is insufficient.

Soak must pass and last at least 86,400 seconds. Reversed timestamps, future completion,
expired results, missing/duplicate/failed suites, mismatched identities, invalid signatures,
unknown top-level fields and oversized documents fail closed. The trusted harness measures
continuous duration and must restart qualification after a failed or interrupted soak. The
supervisor uses its own UTC clock for freshness; clock rollback before completion denies
qualification. All detailed suite and hardware reports must remain retrievable by digest in
the harness's evidence archive; the supervisor stores only bounded safe references.

## Request, persistence and invalidation

An administrator or trusted delivery pipeline copies the exact evidence bytes to
`evidence/<sha256-of-document>.json` and the raw 64-byte signature to the matching `.sig`
file. Finish publishing both files before requesting qualification. Each is opened with
owner/link checks; evidence is limited to 64 KiB. The IPC caller cannot supply a path, key,
test result, UID, or success flag. Inbox retention is administrator-managed; deleting a
referenced document makes qualification unavailable, so retain it through promotion review.

Send one newline-terminated JSON request over the existing authenticated Unix socket:

```json
{"operation":"qualify","digest":"<64 lowercase hex candidate digest>","evidence_digest":"<64 lowercase hex document digest>"}
```

Qualification requires `stage` permission. `read` cannot qualify; `stage` cannot promote.
A valid request returns `ok` only after the activation journal records qualification and
the private request ledger persists the authenticated UID, exact candidate and evidence
reference. Repeating a valid request is safe. An interrupted ledger publication blocks
mutation and exposes recovery status; a journal phase alone never grants eligibility.

Authorized status reads return `qualification` only after rechecking the signature, installed
bytes, current candidate/previous-good identity, policy, expiry and qualified phase. Successful
qualification survives supervisor restart. New candidate bytes invalidate old evidence even
when the release label is unchanged. A failed requalification or staging request clears the
recorded qualification; failed checks never become success observations.

Promotion still requires a separate `activate`-authorized `promote` request and rechecks
qualification. It currently returns `activation_blocked` for a qualified candidate because
idle/drain admission and service stop/start remain separate backlog work (#155–#157).
Unqualified requests return `qualification_required`; neither response changes production.
Bootstrap without an independently established healthy/previous-good production artifact is
also ineligible for normal reversible promotion. No bypass is introduced for first install.

## Verification

`cargo test --locked -p uob-release-manager --test qualification` checks trusted and forged
signatures, signed adversarial claims, exact 24-hour boundaries, input/source mismatches,
failed or incomplete suites and compatibility, restart persistence, authorization separation,
policy revocation, missing evidence, unchanged production pointers, and same-label candidate
replacement. These are deterministic policy fixtures, not a claim that a production candidate
has already completed a 24-hour soak or met Pi resource targets.
