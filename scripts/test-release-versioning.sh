#!/usr/bin/env bash
set -euo pipefail

# Exercise the real release hook with a fresh registry, without credentials or pushes.
repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly temporary_directory="$(mktemp -d)"
trap 'rm -rf -- "$temporary_directory"' EXIT
test "$(cog --version)" = 'cog 7.0.0'
git clone --quiet --no-hardlinks "$repository_root" "$temporary_directory/repository"
cd "$temporary_directory/repository"
git switch --quiet -C main
git config --local user.name 'Release regression test'
git config --local user.email 'release-test@example.invalid'
# Include the working copy of the hook when developers run this before committing.
cp "$repository_root/scripts/update-workspace-version.sh" scripts/update-workspace-version.sh
git add scripts/update-workspace-version.sh
# A version PR already contains the next manifest version before its tag exists.
# Establish that version as the test baseline so the injected fix bumps it again.
manifest_version="$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')"
if ! git rev-parse --verify --quiet "refs/tags/v$manifest_version" >/dev/null; then
  git -c user.name=Test -c user.email=test@example.invalid commit --quiet --allow-empty -m 'chore: version test baseline'
  git tag "v$manifest_version"
fi
git commit --quiet --allow-empty -m 'fix: exercise release generation'
readonly original_revision="$(git rev-parse HEAD)"
export CARGO_HOME="$temporary_directory/cargo-home"
export CARGO_TARGET_DIR="$temporary_directory/target"
cog bump --auto --skip-ci
release_tag="$(git tag --points-at HEAD --list 'v[0-9]*')"
[[ "$release_tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]
cargo metadata --locked --offline --no-deps --format-version 1 >"$temporary_directory/metadata.json"
jq --exit-status --arg version "${release_tag#v}" \
  'all(.packages[]; .version == $version)' "$temporary_directory/metadata.json" >/dev/null
git diff --exit-code
git diff --name-only "$original_revision" HEAD | sort >"$temporary_directory/changed"
printf '%s\n' CHANGELOG.md Cargo.lock Cargo.toml | sort >"$temporary_directory/expected"
diff -u "$temporary_directory/expected" "$temporary_directory/changed"
# Updating workspace versions must not update any third-party locked dependency.
git show "$original_revision:Cargo.lock" >"$temporary_directory/before.lock"
python3 - "$temporary_directory/before.lock" Cargo.lock <<'PY'
import sys
import tomllib

def external_packages(path):
    with open(path, "rb") as source:
        return [package for package in tomllib.load(source)["package"] if "source" in package]

assert external_packages(sys.argv[1]) == external_packages(sys.argv[2])
PY
readonly release_revision="$(git rev-parse HEAD)"
cog bump --auto --skip-ci
test "$(git rev-parse HEAD)" = "$release_revision"
echo 'cold release versioning, locked dependencies, and no-op rerun verified'
