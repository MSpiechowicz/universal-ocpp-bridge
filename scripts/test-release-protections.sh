#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly fixture_source="$repository_root/tests/security-fixtures/release-protection"
readonly temporary_directory="$(mktemp -d)"
trap 'rm -rf -- "$temporary_directory"' EXIT

run_check() {
  GH_REPOSITORY=example/repository \
    RELEASE_PROTECTION_FIXTURES_DIRECTORY="$1" \
    "$repository_root/scripts/check-release-protections.sh"
}

run_check "$fixture_source" >/dev/null

cp -R "$fixture_source/." "$temporary_directory/"
jq 'del(.required_status_checks.contexts[] | select(. == "GitHub Actions policy"))' \
  "$fixture_source/branch.json" >"$temporary_directory/branch.json"

if run_check "$temporary_directory" >"$temporary_directory/output" 2>&1; then
  echo "release protection verifier accepted a missing required check" >&2
  exit 1
fi

grep -Fq \
  'release protection check blocked: main does not require check: GitHub Actions policy' \
  "$temporary_directory/output"

echo "release protection safeguards verified"
