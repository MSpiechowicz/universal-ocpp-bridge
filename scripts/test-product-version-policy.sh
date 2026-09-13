#!/usr/bin/env bash
set -euo pipefail

readonly root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly proposer="$root/scripts/propose-product-version.sh"
readonly temporary="$(mktemp -d)"
trap 'rm -rf -- "$temporary"' EXIT
readonly cog_bin="${COG_BIN:-cog}"
test "$("$cog_bin" --version)" = 'cog 7.0.0'
export COG_BIN="$cog_bin"
export GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_GLOBAL=/dev/null

fixture_time=1700000000
fixture_commit() {
  fixture_time=$((fixture_time + 1))
  GIT_AUTHOR_DATE="$fixture_time +0000" GIT_COMMITTER_DATE="$fixture_time +0000" git commit "$@"
}

new_history() {
  local name="$1" baseline="${2:-1.2.3}"
  history="$temporary/$name"
  git init --quiet --initial-branch=main "$history"
  cd "$history"
  git config user.name 'Version policy fixture'
  git config user.email version-policy@example.invalid
  cp "$root/cog.toml" cog.toml
  mkdir scripts
  # Any accidental non-dry bump fails and leaves evidence, before writing manifests.
  cat > scripts/update-workspace-version.sh <<'HOOK'
#!/usr/bin/env bash
touch hook-executed
exit 97
HOOK
  chmod +x scripts/update-workspace-version.sh
  printf '[workspace.package]\nversion = "%s"\n' "$baseline" > Cargo.toml
  printf '# fixture lockfile\n' > Cargo.lock
  printf '# fixture changelog\n' > CHANGELOG.md
  git add .
  fixture_commit --quiet --message 'chore: baseline'
  if [[ "$baseline" != none ]]; then git tag "v$baseline"; fi
}

snapshot() {
  git rev-parse HEAD
  git symbolic-ref HEAD
  git show-ref
  git status --porcelain --untracked-files=all
  git diff --binary HEAD
  git ls-files -s
}

assert_proposal() {
  local expected_status="$1" expected_version="$2"
  snapshot >"$temporary/before"
  "$proposer" "$history" >"$temporary/result" 2>"$temporary/diagnostic"
  jq --exit-status --arg status "$expected_status" --arg version "$expected_version" \
    --arg source "$(git rev-parse HEAD)" \
    '.status == $status and (.proposed_version // "") == $version
      and .source_revision == $source and .publication_authorized == false' \
    "$temporary/result" >/dev/null || {
      cat "$temporary/result" "$temporary/diagnostic" >&2
      exit 1
    }
  snapshot >"$temporary/after"
  diff -u "$temporary/before" "$temporary/after"
  test ! -e hook-executed
}

for type in fix perf feat; do
  new_history "$type"
  fixture_commit --quiet --allow-empty --message "$type: change"
  expected=1.2.4
  if [[ "$type" == feat ]]; then expected=1.3.0; fi
  assert_proposal proposed "$expected"
done

for type in docs test ci build chore refactor style revert; do
  new_history "$type"
  fixture_commit --quiet --allow-empty --message "$type: maintenance"
  assert_proposal no_release ''
  # Breaking markers override even an explicitly non-releasing maintenance type.
  fixture_commit --quiet --allow-empty --message "$type!: breaking change"
  assert_proposal proposed 2.0.0
done

new_history footer
fixture_commit --quiet --allow-empty --message 'fix: change contract' \
  --message 'BREAKING CHANGE: callers must supply the new identity'
assert_proposal proposed 2.0.0

new_history precedence
fixture_commit --quiet --allow-empty --message 'fix: patch'
fixture_commit --quiet --allow-empty --message 'perf: patch'
fixture_commit --quiet --allow-empty --message 'feat: minor'
assert_proposal proposed 1.3.0
fixture_commit --quiet --allow-empty --message 'refactor!: major'
assert_proposal proposed 2.0.0

new_history already-tagged
assert_proposal no_release ''
fixture_commit --quiet --allow-empty --message 'feat: already released'
git tag v1.3.0
assert_proposal no_release ''
fixture_commit --quiet --allow-empty --message 'docs: after a released feature'
assert_proposal no_release ''

new_history initial none
fixture_commit --quiet --allow-empty --message 'feat!: initial implementation'
assert_proposal readiness_required ''
jq --exit-status '.baseline == null and .reason == "first_stable_decision_required"' \
  "$temporary/result" >/dev/null

new_history internal 0.28.0
fixture_commit --quiet --allow-empty --message 'feat!: internal breaking change'
assert_proposal internal_candidate 0.29.0
jq --exit-status '.baseline == "0.28.0" and .reason == "first_stable_decision_required"' \
  "$temporary/result" >/dev/null

assert_failure() {
  snapshot >"$temporary/before"
  if "$proposer" "$history" >"$temporary/result" 2>"$temporary/diagnostic"; then
    echo 'Calculation failure was accepted as a successful proposal.' >&2
    exit 1
  fi
  test ! -s "$temporary/result"
  snapshot >"$temporary/after"
  diff -u "$temporary/before" "$temporary/after"
  test ! -e hook-executed
}

new_history malformed-config
printf '\ninvalid = [\n' >> cog.toml
git add cog.toml
fixture_commit --quiet --message 'fix: invalid config fixture'
assert_failure

new_history dirty
printf 'changed\n' >> Cargo.toml
assert_failure

new_history untracked
touch untracked-file
assert_failure

new_history tagged-invalid-config
printf '\ninvalid = [\n' >> cog.toml
git add cog.toml
fixture_commit --quiet --message 'fix: invalid tagged config fixture'
git tag v1.2.4
assert_failure

new_history shallow-source
git clone --quiet --depth=1 "file://$history" "$temporary/shallow"
history="$temporary/shallow"
cd "$history"
assert_failure

new_history invalid-history
fixture_commit --quiet --allow-empty --message 'not a conventional commit'
assert_failure

new_history unsupported-branch
git switch --quiet --create topic
fixture_commit --quiet --allow-empty --message 'fix: branch fixture'
assert_failure

new_history candidate-branch
git switch --quiet --create next
fixture_commit --quiet --allow-empty --message 'fix: candidate branch fixture'
assert_failure

new_history wrong-tool
cat > "$temporary/wrong-cog" <<'TOOL'
#!/usr/bin/env bash
echo 'cog 6.0.0'
TOOL
chmod +x "$temporary/wrong-cog"
COG_BIN="$temporary/wrong-cog" assert_failure

new_history unknown-output
cat > "$temporary/unexpected-cog" <<'TOOL'
#!/usr/bin/env bash
if [[ "$1" == --version ]]; then echo 'cog 7.0.0'; else echo 'unexpected output'; fi
TOOL
chmod +x "$temporary/unexpected-cog"
COG_BIN="$temporary/unexpected-cog" assert_failure

echo 'Product version rules, readiness boundary, failure handling, and read-only proposals verified'
