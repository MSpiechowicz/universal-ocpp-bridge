# Frontend pull request checks

The `Frontend checks` workflow runs on every pull request and main push. Its
`Frontend type, lint, build, and actual-service browser tests` check fails if the
frontend package or lockfile is absent, dependencies drift, typechecking, linting,
unit tests, static build, reproducibility, committed-asset comparison, or browser
tests fail. Repository administrators can require this check in branch protection.
It does not publish assets or change release approval policy.

Node 26.8.1, npm 12.0.2 and all direct frontend dependencies are exact pins.
`tooling/package-lock.json` bootstraps npm; `npm ci --ignore-scripts` consumes the
application lockfile. Node setup and npm cache
are confined to the frontend job. Existing Rust daemon, simulator, commit and
release-version checks continue to use committed static assets without Node.
Only HTML, JavaScript and CSS enter the daemon through the existing Rust asset
embedding; the build rejects extra files, source maps and assets over budget.

With Node 26.8.1, run from `frontend` (the first command can use Node's bundled npm):

```text
npm ci --prefix tooling --ignore-scripts
export PATH="$PWD/tooling/node_modules/.bin:$PATH"
npm ci --ignore-scripts
npm run check
npm run check:reproducible
git diff --exit-code -- ../adapters/management/ui
cargo build --locked -p uob-service --bin uob
cargo build --locked -p uob-management-adapter --example browser_fixture
./node_modules/.bin/playwright install --with-deps chromium
npm run test:browser:ci
```

Oxlint checks correctness rules in application code, tests and tooling. The only
generated TypeScript exclusion is the schema bundle, checked by its generator.
Control-character regular expressions remain permitted in the identity and SSE
validators because rejecting those characters is intentional. React Compiler
rules are not enabled: this project does not use that compiler.

The suite starts four real management-router fixtures plus the actual `uob`
executable, built after the asset build, on loopback ports 39189–39193. Occupied
ports fail rather than reusing another process. The daemon uses an isolated
API-only demo configuration without station, broker or external database peers.
Its smoke test verifies compiled assets, runtime identity, credential clearing and
the honest unavailable-query response. Existing router tests cover authenticated
SSE, scope checks, diagnostics and inert rendering. This gate does not claim the
full charging acceptance matrix or implement missing daemon query composition.

## Report boundary and limits

CI uses one worker, no retries, a 30-second test deadline, five-failure cutoff,
ten-minute suite deadline and twelve-minute outer process deadline. The workflow
also limits each temporary browser file to 32 MiB, allowing the existing large
capture-import fixtures while bounding raw output. Retained reports have the much
smaller limit below. CI screenshots, videos and traces
are disabled; manual screenshots in the existing tests are skipped in report-only
mode. Raw stdout/stderr, errors and temporary Playwright output are discarded.
The wrapper kills remaining child processes and removes raw output on completion.

Only `frontend/test-results/ci-summary.json` is uploaded, for seven days. The
report has a 64 KiB limit and retains at most 256 results; exceeding that result
limit fails the check. It includes fixed statuses, integer counts/durations, line
numbers and SHA-256 hashes of source basenames. It excludes test titles, absolute
paths, assertion text, browser payloads, attachments and credentials. To locate a
reported test, hash its source basename (without a newline) and use its line:

```text
node -e 'console.log(require("node:crypto").createHash("sha256").update("daemon.browser.ts").digest("hex"))'
```

A missing report fails the browser command, including startup failures. A local
`npm run test:browser` run provides full diagnostics and optional screenshots for
debugging with public fixture credentials; do not upload those raw outputs.
Focused unit tests exercise deliberately invalid type/lint inputs, oversized build
output, a real failing Playwright run with reflected credentials, and report
overflow/redaction.

The npm bootstrap preserves the project's existing npm 12.0.2 pin. On 2026-09-12,
`npm audit --prefix tooling` reports advisories in its bundled brace-expansion,
ip-address, tar and undici dependencies (five findings including the npm parent).
The application lockfile audit is clean. Updating the package-manager pin requires
a separate reviewed tooling update; the frontend job does not run `npm audit fix`
or silently downgrade tooling.
