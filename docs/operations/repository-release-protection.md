# Repository release protection

The repository settings are part of the release boundary. Workflow YAML requests permissions and
an environment, but it cannot prove that GitHub enforces branch or environment rules. Stable source
publication stays blocked unless the live settings below pass
`scripts/check-release-protections.sh` from inside the protected release job.

## Required GitHub settings

Configure the repository with `main` as the default branch and enable only squash merging. Set the
squash commit title to the pull-request title; disable merge commits and rebase merging. This makes
the Conventional Commit pull-request-title check the source of the feature squash subject.

Protect `main`, including administrators, with all of these rules:

- Require a pull request before merging and require review conversations to be resolved.
- Require linear history and require branches to be up to date before merging.
- Reject force pushes and branch deletion.
- Require `Format, lint, test, and architecture`,
  `PR title, commit range, and documentation`, `Rust advisories, licenses, and sources`,
  `Secret scanning`, `GitHub Actions policy`, and `Locked source SBOM`.
- Do not grant a direct-push bypass. Version changes pass through a reviewed pull request;
  the protected release job only pushes preparation branches and tags on verified main commits.

Set the repository Actions default workflow permission to read-only and prohibit Actions from
approving pull requests. The committed repository checks reject broad workflow permissions,
untrusted privileged triggers, self-hosted runners, unpinned actions, and additional secret use.

Create an environment named `stable-release` with at least one required reviewer. Restrict its
deployment branches to protected branches, and store one environment secret named
`RELEASE_PROTECTION_TOKEN`. That token is not a publishing identity: make it a fine-grained token
limited to this repository with read-only Administration and Environments access, which is needed
to inspect branch, environment, and workflow-permission settings. The publication identity remains
the job-scoped, short-lived `GITHUB_TOKEN`. Configure OIDC instead of long-lived signing or
distribution secrets when a future package registry supports it.

GitHub documents these controls under [protected branches](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/defining-the-mergeability-of-pull-requests/about-protected-branches),
[deployment environments](https://docs.github.com/en/actions/concepts/workflows-and-actions/deployment-environments),
and [secure use of OpenID Connect](https://docs.github.com/en/actions/security-for-github-actions/security-hardening-your-deployments/about-security-hardening-with-openid-connect).

## Verify before enabling publication

An administrator should first run the same fail-closed check used by the release job:

```text
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

The publication workflow never pushes generated commits directly to `main`. It prepares a version
branch and writes a PR comparison link into the job summary. A maintainer opens that PR, allowing
normal pull-request checks to run, and merges it through the existing protections. The next
verified main run publishes only its version tag and GitHub Release. A successful preparation job
means the version is ready for review; it does not mean that version has been published.

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
remote rejects every direct main push. It exercises version preparation, repeated preparation,
publication after a simulated reviewed merge, and recovery when the Release API fails after the
tag is pushed. The real publisher uses only the existing job-scoped `GITHUB_TOKEN`; the protection
check still needs `RELEASE_PROTECTION_TOKEN`.

## Publication acceptance

After the check succeeds, trigger two harmless release-eligible test runs before relying on the
gate. Keep the first run awaiting environment approval, approve it, then start and approve the
second. GitHub must leave the first publication running and queue the second behind the shared
`stable-release-publication` concurrency group. A newer run must not cancel an in-progress
publication.

Do not enable a signing, package publication, or device-deployment step until its immutable input,
protected environment, short-lived identity, and independent verification are implemented. Fork
pull requests remain read-only and cannot feed an artifact or writable cache into this job.
