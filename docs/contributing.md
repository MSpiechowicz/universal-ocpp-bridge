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

Pull requests are feature-squashed into `main`. The pull request title becomes the squash commit
subject, so it must be a valid Conventional Commit before review and after every title edit. Use a
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

Pull request checks do not calculate a product version, create tags, publish artifacts, or comment
on pull requests. After the Rust workspace workflow succeeds for a push to `main`, its release job
uses the same reviewed Cocogitto 7.0.0 binary to calculate the next semantic version from commits
since the latest `v*` tag. A breaking change increments the major version, `feat` increments the
minor version, and `fix` increments the patch version.

The release job updates `[workspace.package].version`, refreshes `Cargo.lock`, creates a generated
changelog, verifies that `uob-service` still builds with the new embedded version, and records all
three files in a `chore(version)` commit tagged with the new version. The commit and tag are pushed
atomically before a GitHub Release is created. Its `[skip ci]` marker prevents the generated commit
from starting the workflow again. The job has write permission only after the exact `main` revision
has passed the complete workspace checks; pull request jobs remain read-only. Commit types that do
not require a semantic-version increment finish successfully without creating a release.

Preview the next version without modifying the repository:

```text
cog bump --auto --dry-run
```

Inspect the source version, latest tag, and identity reported by a running service:

```text
cargo metadata --locked --no-deps --format-version 1 \
  | jq --raw-output '.packages[] | select(.name == "uob-service") | .version'
cog get-version
curl --silent http://127.0.0.1:8080/api/v1/identity \
  | jq '.runtime | {release_id, release_digest}'
```
