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

reject_repository_response() {
  local transformation="$1"
  local diagnostic="$2"
  cp "$fixture_source/branch.json" "$temporary_directory/branch.json"
  jq "$transformation" "$fixture_source/repository.json" >"$temporary_directory/repository.json"
  if run_check "$temporary_directory" >"$temporary_directory/output" 2>&1; then
    echo "release protection verifier accepted invalid repository response: $transformation" >&2
    exit 1
  fi
  grep -Fq "$diagnostic" "$temporary_directory/output"
}

reject_repository_response '.data.repository.mergeCommitAllowed = true' \
  'only squash merging must be enabled'
reject_repository_response '.data.repository.rebaseMergeAllowed = true' \
  'only squash merging must be enabled'
reject_repository_response '.data.repository.squashMergeAllowed = false' \
  'only squash merging must be enabled'
reject_repository_response '.data.repository.squashMergeCommitTitle = "COMMIT_OR_PR_TITLE"' \
  'squash commits must use the pull-request title'
reject_repository_response 'del(.data.repository.squashMergeAllowed)' \
  'repository settings unavailable from GraphQL'
reject_repository_response '.data.repository = null' \
  'repository settings unavailable from GraphQL'
reject_repository_response '.errors = [{message: "Resource not accessible"}]' \
  'repository settings unavailable from GraphQL'
reject_repository_response '.data.repository.mergeCommitAllowed = "false"' \
  'repository settings unavailable from GraphQL'

echo "release protection safeguards verified"
