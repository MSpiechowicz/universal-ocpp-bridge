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
mkdir scripts
cp "$root/scripts/update-workspace-version.sh" scripts/
cat > cog.toml <<'TOML'
branch_whitelist = ["main"]
tag_prefix = "v"
pre_bump_hooks = ["./scripts/update-workspace-version.sh {{version}}"]
TOML
cargo generate-lockfile --offline
git add .
git commit --quiet -m 'feat: initial version'
git tag v0.1.0
git clone --quiet --bare . "$temporary/remote"
cat > "$temporary/remote/hooks/pre-receive" <<'HOOK'
#!/usr/bin/env bash
while read -r old new ref; do
  if [[ "$ref" == refs/heads/main ]]; then
    echo 'main requires a reviewed PR' >&2
    exit 1
  fi
done
HOOK
chmod +x "$temporary/remote/hooks/pre-receive"
cat > "$temporary/bin/gh" <<'GH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == 'release list --limit 100 --json tagName' ]]; then
  if [[ -f "$RELEASE_TEST_STATE/released" ]]; then
    printf '[{"tagName":"v0.2.0"}]\n'
  else
    echo '[]'
  fi
elif [[ "$1 $2" == 'release create' ]]; then
  [[ "$3" == v0.2.0 ]]
  if [[ -f "$RELEASE_TEST_STATE/fail-api" ]]; then exit 1; fi
  touch "$RELEASE_TEST_STATE/released"
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
    git config user.name Test
    git config user.email test@example.invalid
    "$root/scripts/publish-stable-release.sh"
  )
}
# Add a reviewed feature to main without allowing the publication identity to push main.
echo '// feature' >>src/main.rs
git add .
git commit --quiet -m 'feat: next feature'
source_revision="$(git rev-parse HEAD)"
git push --quiet "$temporary/remote" HEAD:refs/heads/reviewed-feature
git --git-dir="$temporary/remote" update-ref refs/heads/main "$source_revision"
run_publication
release_branch="refs/heads/codex/release-v0.2.0-$source_revision"
prepared="$(git --git-dir="$temporary/remote" rev-parse "$release_branch")"
test "$(git --git-dir="$temporary/remote" rev-parse main)" = "$source_revision"
! git --git-dir="$temporary/remote" rev-parse --verify refs/tags/v0.2.0 2>/dev/null
! git --git-dir="$temporary/remote" log -1 --format=%B "$prepared" | grep -F '[skip ci]'
run_publication
test "$(git --git-dir="$temporary/remote" rev-parse "$release_branch")" = "$prepared"
# Model the reviewed version PR merge, then fail the Release API after tag publication.
git --git-dir="$temporary/remote" update-ref refs/heads/main "$prepared"
touch "$temporary/fail-api"
if run_publication; then echo 'accepted failed Release API' >&2; exit 1; fi
test "$(git --git-dir="$temporary/remote" rev-parse refs/tags/v0.2.0)" = "$prepared"
rm "$temporary/fail-api"
run_publication
test -f "$temporary/released"
run_publication
test "$(git --git-dir="$temporary/remote" rev-parse main)" = "$prepared"
echo 'protected-main preparation, tag-only publication, and retry recovery verified'
