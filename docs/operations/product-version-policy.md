# Product version proposals

The product uses the explicit commit rules in `cog.toml`, tested with Cocogitto 7.0.0.
Run the following in a clean, full-history `main` checkout with the reviewed release tags
already present. The command requires Bash, Git, jq and the pinned Rust-native `cog` binary:

```text
./scripts/propose-product-version.sh
```

An optional repository path selects another local checkout. The command does not fetch tags,
switch branches, execute bump hooks, edit files, create commits or tags, push, or publish to a
package registry. It invokes `cog bump --auto --dry-run`; repository changes, shallow history,
an unsupported branch, an invalid configuration/history, or the wrong tool version fail with
a nonzero exit and no success JSON. Unknown calculator output also fails. The exact successful
no-change output of 7.0.0 is handled separately from every command failure.

## Commit rules

After an established stable baseline with major version at least one:

| Commit | Proposal from 1.2.3 |
|---|---|
| `fix: correct a failure` | 1.2.4 |
| `perf: reduce allocations` | 1.2.4 |
| `feat: add a capability` | 1.3.0 |
| `feat!: change the contract` | 2.0.0 |
| Any recognized type with a `BREAKING CHANGE:` footer | 2.0.0 |
| Ordinary `docs`, `test`, `ci`, `build`, `chore`, `refactor`, `style`, `revert` | No release |

Breaking markers override the maintenance-only rule. The highest effect wins across the
commit range, and already-tagged changes are not counted again. A revert is treated by its
declared Conventional Commit type; cancellation of earlier version effects is not inferred.
The wrapper explicitly compares the baseline tag target to HEAD: 7.0.0 can otherwise propose
another bump for an already-tagged release-eligible HEAD. Calculator errors still fail before
this no-release result is considered.

## Machine result and first stable release

One JSON object is written to stdout. `baseline` and `proposed_version` are SemVer strings or
null; `source_revision` is the exact Git commit. `publication_authorized` is always false.

| Status | Meaning |
|---|---|
| `proposed` | Eligible changes after an established stable release; qualification and publication are separate. |
| `no_release` | No eligible changes, including an already-tagged HEAD; no new version. |
| `internal_candidate` | A 0.x proposal, which does not establish full product readiness. |
| `readiness_required` | No established stable-format tag; the calculator's default initial version is discarded. |

Cocogitto 7.0.0 keeps automatic breaking bumps within 0.x (for example, 0.28.0 to 0.29.0).
The first full stable 1.0.0 is an explicit readiness decision after the complete release matrix
passes, never an automatic result of the initial-version fallback. Existing 0.x source tags
remain internal preview baselines. Candidate identifiers/channel metadata must mark those
artifacts accordingly. This command has no readiness override or publication action.
Prerelease numbering and promotion of the same candidate are a separate policy; this stable
proposal wrapper rejects prerelease-shaped calculator output.

## Cargo and artifact identity

Keep the existing Cargo synchronization policy: the workspace package version covers the
daemon, contracts, and matching separately packaged simulator. The browser displays the daemon's
runtime release identity and is bundled with that application artifact; its private npm package
version is a build-tool placeholder, not an independently released product. The release manager
retains its separately declared package version and lifecycle. Public JSON schema major/revision
fields change only under the schema compatibility policy, independently of product SemVer.

Proposals never synchronize manifests. The existing explicit release bump hook updates
`Cargo.toml`, the workspace package entries in `Cargo.lock`, and the changelog during the separate
release operation. It preserves third-party locked versions and the release manager version;
`test-release-versioning.sh` verifies these properties. No crate-registry publication is needed.

SemVer, source revision, and artifact digest identify different things. The proposal knows only
the first two and does not manufacture an artifact digest. Qualification binds a built artifact's
real digest; channel promotion must refer to that same digest without changing an embedded version
and rebuilding it. A larger version does not establish rollback compatibility.

## Verification

```text
./scripts/test-product-version-policy.sh
./scripts/test-release-versioning.sh
```

The policy suite creates isolated Git histories with the real pinned calculator. It checks every
documented commit type, marker/footer precedence, mixed changes, already-tagged/no-change history,
initial and 0.x behavior, and failure cases. Before/after comparisons cover refs, HEAD, branch,
index, tracked contents and untracked files; a failing hook sentinel proves proposals skip hooks.
The Conventional Commit workflow runs this suite on pull requests without publication credentials.
