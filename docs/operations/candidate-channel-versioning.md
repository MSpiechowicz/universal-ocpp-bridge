# Candidate channel versioning

`scripts/propose-candidate-channel.py` calculates read-only JSON proposals using the pinned
Cocogitto 7.0.0 binary. `next` is the optional candidate branch; `main` remains the stable branch.
The existing stable-only proposer explicitly requires `main`. Neither calculator runs bump hooks,
edits manifests, commits, tags, pushes, signs evidence, or authorizes publication.

From a clean, full-history `next` checkout, after building an immutable artifact outside the
checkout, calculate its candidate identity:

```text
python3 -B scripts/propose-candidate-channel.py candidate \
  --artifact /path/to/immutable-runtime.tar.gz > /path/to/candidate.json
```

The proposal binds the source revision, Git tree and SHA-256 of the actual artifact bytes. Hashing
streams the artifact in 1 MiB chunks. The source revision is the build identity; the channel version
is external metadata. A later stable association must not rewrite embedded versions or rebuild the
artifact. Packaging, signatures, trusted qualification and publication are downstream gates.
The JSON always reports `publication_authorized: false` and `qualification_verified: false`:
self-supplied JSON is never proof that qualification passed.

## Numbering and readiness

The calculator validates the real `cog bump --auto --dry-run` result against the last stable tag;
`cog get-version --tag` excludes prereleases in the pinned release. Candidate proposals use
`cog bump --auto --dry-run --pre 'rc.*'`. For example, a feature after `v1.2.3` proposes
`1.3.0-rc.1`; subsequent candidate commits propose `1.3.0-rc.2`. Stable and candidate tags have
separate strict formats. A candidate tag collision, including one on a divergent branch, fails.

Cocogitto 7.0.0 increments `rc.*` even at an already tagged candidate commit. The wrapper instead
returns that existing candidate with `existing_candidate` status. It does not allocate another
number for an unchanged rerun. Artifact digests still require comparison with retained trusted
qualification before any publication; an existing tag alone does not authenticate artifact bytes.
At a stable baseline or with no eligible changes it reports `no_release`.

Without an established stable-format baseline the result is `readiness_required`, with no version.
A 0.x baseline permits explicitly internal prerelease metadata, for example `0.29.0-rc.1`, and
sets `first_stable_readiness_required`. Internal candidates cannot become a first stable release
through this calculator. The separate complete-product readiness gate must establish 1.0.0.

## Ancestry-preserving promotion

After downstream gates retain the candidate proposal, artifact and candidate tag, promotion must
preserve candidate ancestry into `main`. A fast-forward or a merge with the identical source tree
is eligible for calculation; a squash with the same tree still fails ancestry validation.

```text
python3 -B scripts/propose-candidate-channel.py promote \
  --artifact /path/to/immutable-runtime.tar.gz \
  --candidate /path/to/candidate.json
```

The candidate tag must resolve to the retained source revision, that revision must be an ancestor
of current `main`, and the candidate, recorded and current trees must match. The actual artifact
SHA-256 must match the retained digest. Changed source or artifact bytes require a new candidate
and qualification. The stable base is taken from the candidate (`1.3.0-rc.2` becomes `1.3.0`),
checked against the stable calculation, and never incremented a second time. An existing stable tag
must preserve both the same tree and candidate ancestry. `build_source_revision` remains the
candidate commit even when `main` contains an ancestry-preserving merge commit.

The required Conventional Commit workflow rejects `next` → `main` PRs through
`scripts/check-channel-promotion.sh`: feature squash validation cannot authorize channel promotion.
Feature PRs into either branch still use the validated Conventional Commit title. The downstream
trusted channel workflow must provide the ancestry-preserving promotion path; this task neither
enables that workflow nor changes repository merge settings. The existing source-publication path
is separate and must not be used to rebuild a qualified candidate for a stable association.

## Verification

Run `python3 -B scripts/test_candidate_channels.py` and
`./scripts/test-product-version-policy.sh`. The Conventional Commit CI workflow runs both.
Synthetic histories exercise sequential candidates, tagged reruns, stable baselines, internal
readiness, fast-forward and merge promotion, squash rejection, artifact/tree mismatch and invalid
state. Every calculation snapshots refs, HEAD, index and working tree and rejects hook execution.
Tests create tags only within their disposable fixture repositories; calculators never create them.
