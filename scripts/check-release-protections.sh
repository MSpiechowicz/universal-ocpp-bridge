#!/usr/bin/env bash
set -euo pipefail

readonly repository="${GH_REPOSITORY:?GH_REPOSITORY must name the owner/repository to verify}"
readonly token="${GH_TOKEN:-}"
readonly api_url="${GH_API_URL:-https://api.github.com}"
readonly fixture_directory="${RELEASE_PROTECTION_FIXTURES_DIRECTORY:-}"
readonly branch="main"
readonly environment="stable-release"
readonly temporary_directory="$(mktemp -d)"
trap 'rm -rf -- "$temporary_directory"' EXIT

request() {
  local endpoint="$1"
  local output="$2"
  local fixture="$3"
  local request_url="$api_url/repos/$repository"

  if [[ -n "$fixture_directory" ]]; then
    cp "$fixture_directory/$fixture" "$output"
    return
  fi

  if [[ -z "$token" ]]; then
    echo "release protection check blocked: GH_TOKEN is missing" >&2
    echo "provide read-only repository Administration and Environments access" >&2
    exit 1
  fi

  if [[ -n "$endpoint" ]]; then
    request_url="$request_url/$endpoint"
  fi

  if ! curl --fail-with-body --silent --show-error --location \
    --proto '=https' --tlsv1.2 \
    --header 'Accept: application/vnd.github+json' \
    --header "Authorization: Bearer $token" \
    --header 'X-GitHub-Api-Version: 2022-11-28' \
    "$request_url" >"$output"; then
    echo "release protection check blocked: GitHub did not expose $endpoint" >&2
    echo "verify that RELEASE_PROTECTION_TOKEN has read-only Administration and Environments access" >&2
    exit 1
  fi
}

expect() {
  local document="$1"
  local expression="$2"
  local failure="$3"

  if ! jq --exit-status "$expression" "$document" >/dev/null; then
    echo "release protection check blocked: $failure" >&2
    status=1
  fi
}

repository_document="$temporary_directory/repository.json"
branch_document="$temporary_directory/branch.json"
environment_document="$temporary_directory/environment.json"
actions_document="$temporary_directory/actions.json"

request '' "$repository_document" repository.json
request "branches/$branch/protection" "$branch_document" branch.json
request "environments/$environment" "$environment_document" environment.json
request 'actions/permissions/workflow' "$actions_document" actions.json

status=0

expect "$repository_document" \
  '.default_branch == "main"' \
  'the default branch is not main'
expect "$repository_document" \
  '.allow_squash_merge == true and .allow_merge_commit == false and .allow_rebase_merge == false' \
  'only squash merging must be enabled'
expect "$repository_document" \
  '.squash_merge_commit_title == "PR_TITLE"' \
  'squash commits must use the pull-request title'

expect "$branch_document" \
  '.required_status_checks.strict == true' \
  'main must require an up-to-date branch before merging'
expect "$branch_document" \
  '.required_pull_request_reviews != null' \
  'main must require changes to pass through a pull request'
expect "$branch_document" \
  '.enforce_admins.enabled == true' \
  'main protections must include administrators'
expect "$branch_document" \
  '.required_conversation_resolution.enabled == true' \
  'main must require review-conversation resolution'
expect "$branch_document" \
  '.required_linear_history.enabled == true' \
  'main must require linear history'
expect "$branch_document" \
  '.allow_force_pushes.enabled == false and .allow_deletions.enabled == false' \
  'main must reject force pushes and deletion'

required_checks=(
  'Format, lint, test, and architecture'
  'PR title, commit range, and documentation'
  'Rust advisories, licenses, and sources'
  'Secret scanning'
  'GitHub Actions policy'
  'Locked source SBOM'
)
for check in "${required_checks[@]}"; do
  jq --arg check "$check" \
    --exit-status '.required_status_checks.contexts | index($check) != null' \
    "$branch_document" >/dev/null || {
      echo "release protection check blocked: main does not require check: $check" >&2
      status=1
    }
done

expect "$environment_document" \
  '[.protection_rules[].type] | index("required_reviewers") != null' \
  'stable-release must require an explicit reviewer'
expect "$environment_document" \
  '.deployment_branch_policy.protected_branches == true and .deployment_branch_policy.custom_branch_policies == false' \
  'stable-release must accept deployments only from protected branches'
expect "$actions_document" \
  '.default_workflow_permissions == "read" and .can_approve_pull_request_reviews == false' \
  'Actions must default to read-only and must not approve pull requests'

if (( status != 0 )); then
  exit "$status"
fi

echo "release protections verified for $repository"
