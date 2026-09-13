# Contributing

## Conventional commit and pull request policy

The repository uses [Cocogitto](https://github.com/cocogitto/cocogitto) 7.0.0 to validate
[Conventional Commits](https://www.conventionalcommits.org/). Install exactly the reviewed version
with Rust 1.88 or newer:

```text
rustup toolchain install 1.88.0
cargo +1.88.0 install --locked cocogitto --version '=7.0.0'
cog --version
```

The reported version must be `cog 7.0.0`. Cocogitto is a development and release tool; it is
not linked into the bridge, installed on a charging device, or used to publish a release by these
checks. CI instead downloads Cocogitto's official static Linux release archive and verifies the
reviewed SHA-256 digest before executing it. This keeps the application's Rust 1.98 toolchain pin
unchanged and avoids resolving a newer tool-only transitive dependency during every check. The
workflow also supplies the checkout-local author identity that Cocogitto 7.0.0 requires when
rendering verification results; it does not create a commit or grant write permissions.

Use Conventional Commit messages such as:

```text
feat(api): add a command endpoint
fix(ocpp): reject a duplicate identifier
docs: explain local validation
feat(api)!: change command semantics
```

Validate one proposed pull request title locally without placing it in executable shell text:

```text
PR_TITLE='feat(api): add a command endpoint' ./scripts/check-conventional.sh title
```

Validate commits added after the adoption baseline:

```text
./scripts/check-conventional.sh range 05c259f5892cab55b3b246a53e07ff41ffeb656d "$(git rev-parse HEAD)"
```

Commit `05c259f5892cab55b3b246a53e07ff41ffeb656d` is the explicit adoption baseline. Earlier history is
not retroactively validated. Pull request CI uses the pull request base and head SHAs, so unrelated
historical commits do not block new work. The checker calculates their merge base and passes that
bounded range to `cog check --ignore-merge-commits`, so genuine Git merge commits do not block a
range while malformed ordinary commits still fail. `cog.toml` deliberately keeps merge-message
ignoring disabled for standalone title verification.

Feature pull requests are feature-squashed into `main` (or optional `next`).
Channel promotion from `next` to `main` is rejected by this feature PR path; it requires the
separate trusted ancestry-preserving gate described in the
[candidate channel policy](operations/candidate-channel-versioning.md).
The feature pull request title becomes the squash commit subject, so it must be a valid Conventional Commit before review and after every title edit. Use a
`!` before the colon or a `BREAKING CHANGE:` footer only for an intentional breaking change. Merge
commits used to synchronize branches are not squash-title substitutes.

The CI workflow has read-only repository permissions. It transfers the untrusted pull request title
through a GitHub Actions environment value and a temporary file consumed by `cog verify --file`;
shell metacharacters in a title remain literal data. The workflow runs for opened, synchronized,
edited, and reopened pull requests, checks the title, checks only the base-to-head commit range, and
runs the repository documentation checks.

Run the focused acceptance checks with:

```text
./scripts/test-conventional-checks.sh
```

Run all current workspace, architecture, and documentation checks with:

```text
./scripts/verify-workspace.sh
```

Frontend changes also require the isolated [frontend checks](testing/frontend-checks.md),
including the pinned build and browser suite against the actual daemon and management fixtures.

Pull request checks exercise version proposals in disposable fixture histories. They do not
create product tags, publish artifacts, or comment on pull requests. After the Rust workspace
workflow succeeds for a push to `main`, its release job
uses the same reviewed Cocogitto 7.0.0 binary to calculate the next semantic version from commits
since the latest `v*` tag. After stable 1.0.0, a breaking change increments the major version,
`feat` increments the minor version, and `fix` or `perf` increments the patch version.
Maintenance-only changes do not require a release. See the
[product version policy](operations/product-version-policy.md) for the tested rules, 0.x behavior,
first-stable readiness boundary, and Cargo/schema/supervisor version ownership.

The existing protected source-publication job is separate from proposal generation. When the
verified main revision still uses an already released version and eligible changes exist, it runs
the explicit bump hook, verifies the generated workspace version, lockfile and changelog, and
atomically publishes the release commit and tag through the approved release identity.

When main contains an unreleased workspace version, the protected job verifies its build and
pushes only a version tag pointing to that exact main revision, then creates the GitHub Release.
Retries recover a missing GitHub Release after a successful tag push; superseded main runs skip
publication. Changes that do not require a version increment finish successfully without a bump.
Source publication is not qualification or production activation, and a version proposal grants
no publication permission.

Preview the next version as JSON from a clean, full-history `main` checkout:

```text
./scripts/propose-product-version.sh
```

Inspect the source version, latest tag, and identity reported by a running service:

```text
cargo metadata --locked --no-deps --format-version 1 \
  | jq --raw-output '.packages[] | select(.name == "uob-service") | .version'
cog get-version
curl --silent http://127.0.0.1:8080/api/v1/identity \
  | jq '.runtime | {release_id, release_digest}'
```
