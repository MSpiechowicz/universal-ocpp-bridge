#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly fixture_source="$repository_root/tests/security-fixtures/release-protection"
readonly temporary_directory="$(mktemp -d)"
trap 'rm -rf -- "$temporary_directory"' EXIT

run_check() {
  GH_REPOSITORY=example/repository \
    RELEASE_RULESET_ID=1 RELEASE_APP_ID=12345 \
    RELEASE_PROTECTION_FIXTURES_DIRECTORY="$1" \
    "$repository_root/scripts/check-release-protections.sh"
}

run_check "$fixture_source" >/dev/null

cp -R "$fixture_source/." "$temporary_directory/"
jq 'del(.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[] | select(.context == "GitHub Actions policy"))' \
  "$fixture_source/ruleset.json" >"$temporary_directory/ruleset.json"

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
  cp "$fixture_source/ruleset.json" "$temporary_directory/ruleset.json"
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

reject_ruleset() {
  local transformation="$1"
  local diagnostic="$2"
  cp "$fixture_source/repository.json" "$temporary_directory/repository.json"
  jq "$transformation" "$fixture_source/ruleset.json" >"$temporary_directory/ruleset.json"
  if run_check "$temporary_directory" >"$temporary_directory/output" 2>&1; then
    echo "release protection verifier accepted invalid ruleset: $transformation" >&2
    exit 1
  fi
  grep -Fq "$diagnostic" "$temporary_directory/output"
}
reject_ruleset '.enforcement = "disabled"' 'release ruleset must actively protect main'
reject_ruleset '.conditions.ref_name.exclude = ["refs/heads/main"]' 'release ruleset must actively protect main'
reject_ruleset 'del(.rules[] | select(.type == "pull_request"))' 'release ruleset must require pull requests'
reject_ruleset '(.rules[] | select(.type == "required_status_checks").parameters.strict_required_status_checks_policy) = false' \
  'release ruleset must require pull requests'
reject_ruleset '.bypass_actors[0].actor_id = 999' 'only the configured release App'
reject_ruleset '.bypass_actors[0].bypass_mode = "pull_request"' 'only the configured release App'
reject_ruleset '.bypass_actors += [{actor_id: 5, actor_type: "RepositoryRole", bypass_mode: "always"}]' \
  'only the configured release App'
# GitHub may redact bypass actors for the read-only verifier token.
jq 'del(.bypass_actors)' "$fixture_source/ruleset.json" >"$temporary_directory/ruleset.json"
run_check "$temporary_directory" >/dev/null
# A leftover classic PR requirement would block the release identity.
jq '.required_pull_request_reviews = {}' "$fixture_source/branch.json" >"$temporary_directory/branch.json"
if run_check "$temporary_directory" >"$temporary_directory/output" 2>&1; then
  echo 'accepted conflicting classic protection' >&2; exit 1
fi
grep -Fq 'move main review and status-check requirements' "$temporary_directory/output"
echo 'release ruleset and dedicated App safeguards verified'
