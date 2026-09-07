# Repository release protection

The repository settings are part of the release boundary. Workflow YAML requests permissions and
an environment, but it cannot prove that GitHub enforces branch or environment rules. Stable source
publication stays blocked unless the live settings below pass
`scripts/check-release-protections.sh` from inside the protected release job.

## Required GitHub settings

Configure the repository with `main` as the default branch and enable only squash merging. Set the
squash commit title to the pull-request title; disable merge commits and rebase merging. This makes
the Conventional Commit pull-request-title check the source of the feature squash subject.

Keep classic protection on `main`, including administrators, for linear history, review-conversation
resolution, rejection of force pushes, and rejection of branch deletion. Move the pull-request and
required-status-check requirements into an active branch ruleset targeting exactly `refs/heads/main`.
The ruleset must require up-to-date branches and these checks:

- `Format, lint, test, and architecture`
- `PR title, commit range, and documentation`
- `Rust advisories, licenses, and sources`
- `Secret scanning`
- `GitHub Actions policy`
- `Locked source SBOM`

Grant only the dedicated release GitHub App an **Always** bypass on that ruleset. Do not grant it a
bypass on the remaining classic structural protections. Everyone else, including administrators,
continues to use a PR and required checks. Do not leave duplicate PR or status requirements in the
classic protection: those would also block the App's generated version commit.

Set the repository Actions default workflow permission to read-only and prohibit Actions from
approving pull requests. The committed repository checks reject broad workflow permissions,
untrusted privileged triggers, self-hosted runners, unpinned actions, and additional secret use.

Create an environment named `stable-release` with at least one required reviewer. Restrict its
deployment branches to protected branches, and store one environment secret named
`RELEASE_PROTECTION_TOKEN`. That token is not a publishing identity: make it a fine-grained token
limited to this repository with read-only Administration and Environments access, which is needed
to inspect branch, environment, and workflow-permission settings. The publishing identity is a dedicated GitHub App installed only on this repository with
**Contents: read and write** permission. In `stable-release`, store its numeric App ID as the
`RELEASE_APP_ID` variable, its private key as `RELEASE_APP_PRIVATE_KEY`, and the review/check ruleset
ID as `RELEASE_RULESET_ID`. The pinned official token action requests a repository-scoped,
short-lived installation token after approval and revokes it when the job finishes. The read-only
protection token must not be reused as the publishing identity.

To migrate an existing installation:

1. Create and install the dedicated App on this repository, with only Contents write permission.
   Disable webhooks; this App is used only for workflow authentication.
2. Save the private key in the protected environment secret and set `RELEASE_APP_ID`.
3. Create the active main ruleset above, preserving existing review requirements and the checks'
   GitHub Actions integration binding. Add only this App to its Always bypass list.
4. Set `RELEASE_RULESET_ID` in the protected environment. Verify the active ruleset before removing
   the duplicate classic PR and status requirements; retain all other classic protections.
5. Run the live verifier below with both IDs. Then merge the workflow change through the ruleset's
   normal PR checks. No version PR is required for subsequent releases.

Steps 3–4 can be performed by `GH_REPO=owner/repository
./scripts/configure-release-protections.sh <app-id>` from an authenticated administrator shell.
The helper copies the current review/check policy, verifies the new active ruleset, sets the
protected environment variables, and removes only the duplicate classic requirements. It refuses
a repeated or partial migration for administrator inspection. It never handles the App private key.

GitHub can omit the bypass list from read-only ruleset responses. The verifier checks the exact App
exception when that field is exposed; otherwise an administrator must verify the bypass list during
setup. Missing rule, branch, or environment data still blocks publication.

GitHub documents these controls under [protected branches](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/defining-the-mergeability-of-pull-requests/about-protected-branches),
[deployment environments](https://docs.github.com/en/actions/concepts/workflows-and-actions/deployment-environments),
and [secure use of OpenID Connect](https://docs.github.com/en/actions/security-for-github-actions/security-hardening-your-deployments/about-security-hardening-with-openid-connect).

## Verify before enabling publication

An administrator should first run the same fail-closed check used by the release job:

```text
RELEASE_APP_ID=<numeric-app-id> RELEASE_RULESET_ID=<numeric-ruleset-id> \
GH_REPOSITORY=MSpiechowicz/universal-ocpp-bridge \
GH_TOKEN="$(gh auth token)" \
./scripts/check-release-protections.sh
```

Use a token with read-only Administration and Environments access. The script does not print the
token or mutate settings. It reports each missing protection separately. An authentication or API
error is also a failure; it is never interpreted as an absent optional feature.

The verifier reads repository merge settings through GraphQL because the REST repository response
can omit those fields for read-only tokens. Branch protections, environment rules, and Actions
permissions still use REST. Missing GraphQL fields or errors block verification with an unavailable
settings diagnostic; they are not reported as incorrect merge settings. Keep the protection token
read-only. `GH_GRAPHQL_URL` can override the default `https://api.github.com/graphql` endpoint.

Run the focused offline acceptance fixtures with `./scripts/test-release-protections.sh`. They prove
the complete policy is accepted and that removing a required check produces a named, blocking
failure without requiring repository administration access.

After the verified main build, one `stable-release` approval runs Cocogitto's automatic version
check. Release-eligible commits produce a version commit updating only `Cargo.toml`, `Cargo.lock`,
and `CHANGELOG.md`. The job checks the resulting workspace, then atomically pushes `main` and its
new `vX.Y.Z` tag and creates the GitHub Release. The generated commit includes `[skip ci]` so it does
not start another approval cycle. No-op changes do not create a version commit or tag. An already
reviewed, unreleased manifest version is published directly for compatibility with the old flow.

The publisher rejects changes outside version metadata and the changelog, dependency changes,
independent package-version changes, and non-increasing versions. A non-fast-forward push fails
without publishing either ref. A `Release-Source` trailer allows a rerun of the original approved
revision to recover a failed Release API call when its generated commit is still main's tip.
Superseded runs otherwise skip publication. This publishes source releases, not deployed binaries.

## Clean-runner version verification

The version hook first fetches the existing locked dependency graph, then updates workspace
versions offline. This bootstrap is required because the protected release job intentionally does
not restore a shared Cargo cache. Its timeout includes dependency downloads and a fresh build.

Run `./scripts/test-release-versioning.sh` with Cocogitto 7.0.0, Cargo, Python 3.11 or newer, Git,
and jq installed. It clones into a temporary directory and uses an empty Cargo home, so network
access to the locked dependencies is required. It checks the real version hook, all workspace
package versions, the three generated files, unchanged third-party dependencies, and a no-op
second bump. It never pushes or accesses GitHub credentials. The conventional workflow runs this
regression test on pull requests, before a release can reach its protected environment.

Run `./scripts/test-stable-publication.sh` for the offline publication acceptance test. Its local
remote accepts only a generated direct-child version commit with the source trailer and CI-skip
marker. It exercises feature and fix bumps, no-op changes, atomic push rejection, superseded runs,
and API recovery from the original approved checkout. It uses the real Cocogitto version hook and
Cargo checks with a stubbed GitHub Release API; live GitHub App authorization still requires setup.

## Publication acceptance

After the check succeeds, trigger two harmless release-eligible test runs before relying on the
gate. Keep the first run awaiting environment approval, approve it, then start and approve the
second. GitHub must leave the first publication running and queue the second behind the shared
`stable-release-publication` concurrency group. A newer run must not cancel an in-progress
publication.

Do not enable a signing, package publication, or device-deployment step until its immutable input,
protected environment, short-lived identity, and independent verification are implemented. Fork
pull requests remain read-only and cannot feed an artifact or writable cache into this job.
