#!/usr/bin/env bash
set -euo pipefail
readonly root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly temporary="$(mktemp -d)"
trap 'rm -rf -- "$temporary"' EXIT
export GITHUB_STEP_SUMMARY="$temporary/summary"
export GH_REPO=example/repository
export RELEASE_TEST_STATE="$temporary"
export CARGO_HOME="$temporary/cargo"
export CARGO_TARGET_DIR="$temporary/target"
mkdir -p "$temporary/seed/src" "$temporary/bin"
cd "$temporary/seed"
git init --quiet -b main
git config user.name Test
git config user.email test@example.invalid
cat > Cargo.toml <<'TOML'
[workspace]
members = ["."]
[workspace.package]
version = "0.1.0"
[package]
name = "uob-service"
version.workspace = true
edition = "2024"
TOML
echo 'fn main() {}' > src/main.rs
printf '# Changelog\n\n- - -\n' > CHANGELOG.md
mkdir scripts
cp "$root/scripts/update-workspace-version.sh" "$root/scripts/verify-release-diff.py" scripts/
cat > cog.toml <<'TOML'
branch_whitelist = ["main"]
tag_prefix = "v"
pre_bump_hooks = ["./scripts/update-workspace-version.sh {{version}}"]
[changelog]
path = "CHANGELOG.md"
TOML
cargo generate-lockfile --offline
git add .
git commit --quiet -m 'feat: initial version'
git tag v0.1.0
git clone --quiet --bare . "$temporary/remote"
cat > "$temporary/remote/hooks/pre-receive" <<'HOOK'
#!/usr/bin/env bash
set -euo pipefail
while read -r old new ref; do
  if [[ "$ref" == refs/heads/main ]]; then
    # Model an authorized release identity plus a server-side rejection/race.
    [[ ! -f "$RELEASE_TEST_STATE/reject-push" ]] || exit 1
    [[ "$(git rev-parse "$new^")" == "$old" ]]
    [[ "$(git log -1 --format='%(trailers:key=Release-Source,valueonly)' "$new")" == "$old" ]]
    git log -1 --format=%B "$new" | grep -Fq '[skip ci]'
  fi
done
HOOK
chmod +x "$temporary/remote/hooks/pre-receive"
cat > "$temporary/bin/gh" <<'GH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == 'release list --limit 100 --json tagName' ]]; then
  if [[ -f "$RELEASE_TEST_STATE/releases" ]]; then
    jq --raw-input --slurp 'split("\n") | map(select(length > 0) | {tagName: .})' "$RELEASE_TEST_STATE/releases"
  else
    echo '[]'
  fi
elif [[ "$1 $2" == 'release create' ]]; then
  [[ ! -f "$RELEASE_TEST_STATE/fail-api" ]] || exit 1
  echo "$3" >>"$RELEASE_TEST_STATE/releases"
else
  echo "unexpected gh command: $*" >&2
  exit 1
fi
GH
chmod +x "$temporary/bin/gh"
export PATH="$temporary/bin:$PATH"
run_publication() {
  local checkout
  checkout="$(mktemp -d "$temporary/checkout.XXXXXX")"
  git clone --quiet "$temporary/remote" "$checkout"
  (
    cd "$checkout"
    if [[ -n "${1:-}" ]]; then git switch --quiet --detach "$1"; fi
    git config user.name Test
    git config user.email test@example.invalid
    "$root/scripts/publish-stable-release.sh"
  )
}
main_revision() { git --git-dir="$temporary/remote" rev-parse main; }
merge_change() {
  git fetch --quiet "$temporary/remote" main --tags
  git reset --quiet --hard FETCH_HEAD
  echo "// $1" >>src/main.rs
  git add .
  git commit --quiet -m "$1"
  git push --quiet "$temporary/remote" HEAD:refs/heads/reviewed-feature
  git --git-dir="$temporary/remote" update-ref refs/heads/main "$(git rev-parse HEAD)"
}
# Already released source: idempotently publish the existing tag, no bump.
run_publication
initial="$(main_revision)"
run_publication
test "$(main_revision)" = "$initial"
test "$(wc -l < "$temporary/releases")" = 1
# Documentation-only changes require no new release.
merge_change 'docs: clarify behavior'
no_release="$(main_revision)"
run_publication
test "$(main_revision)" = "$no_release"
test "$(wc -l < "$temporary/releases")" = 1
# A feature needs a minor bump. A rejected atomic push must publish neither ref.
merge_change 'feat: next feature'
source_revision="$(main_revision)"
touch "$temporary/reject-push"
if run_publication; then echo 'accepted rejected push' >&2; exit 1; fi
test "$(main_revision)" = "$source_revision"
! git --git-dir="$temporary/remote" rev-parse --verify refs/tags/v0.2.0 2>/dev/null
rm "$temporary/reject-push"
# One approval updates main and the tag; recover API failure from the OLD source.
touch "$temporary/fail-api"
if run_publication; then echo 'accepted failed Release API' >&2; exit 1; fi
released="$(main_revision)"
test "$released" != "$source_revision"
test "$(git --git-dir="$temporary/remote" rev-parse v0.2.0)" = "$released"
rm "$temporary/fail-api"
run_publication "$source_revision"
grep -Fxq v0.2.0 "$temporary/releases"
run_publication "$source_revision"
run_publication
test "$(wc -l < "$temporary/releases")" = 2
test "$(main_revision)" = "$released"
# A later fix gets a patch bump, while a stale approved run must not publish it.
merge_change 'fix: correct behavior'
next_source="$(main_revision)"
run_publication "$source_revision"
test "$(main_revision)" = "$next_source"
run_publication
test "$(git --git-dir="$temporary/remote" rev-parse v0.2.1)" = "$(main_revision)"
grep -Fxq v0.2.1 "$temporary/releases"
test "$(wc -l < "$temporary/releases")" = 3
# An unexpected source edit in the generated release is forbidden.
git fetch --quiet "$temporary/remote" main --tags
git reset --quiet --hard FETCH_HEAD
echo '// unauthorized generated edit' >>src/main.rs
git add .
git commit --quiet -m 'chore: invalid release'
if python3 scripts/verify-release-diff.py HEAD^ HEAD; then
  echo 'accepted source changes in release' >&2; exit 1
fi
echo 'single-approval main/tag publication, no-op, atomic rejection, and retry recovery verified'
