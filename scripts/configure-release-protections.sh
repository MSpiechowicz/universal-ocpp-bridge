#!/usr/bin/env bash
set -euo pipefail
# Run once as a repository administrator, after installing the release App and
# saving RELEASE_APP_PRIVATE_KEY in the stable-release environment.
if [[ $# != 1 || ! "$1" =~ ^[1-9][0-9]*$ ]]; then
  echo 'usage: GH_REPO=owner/repository configure-release-protections.sh <app-id>' >&2
  exit 2
fi
readonly app_id="$1"
readonly repository="${GH_REPO:?GH_REPO is required}"
readonly temporary="$(mktemp -d)"
trap 'rm -rf -- "$temporary"' EXIT

gh api "repos/$repository/branches/main/protection" >"$temporary/branch.json"
# Require the current policy before creating its replacement. Refuse partial
# migrations: an administrator should inspect and finish those explicitly.
jq --exit-status '.required_pull_request_reviews != null and
  .required_status_checks.strict == true and
  (.required_status_checks.checks | length > 0)' "$temporary/branch.json" >/dev/null

gh api "repos/$repository/rulesets?per_page=100" >"$temporary/rulesets.json"
if jq --exit-status 'any(.[]; .name == "Reviewed main changes")' "$temporary/rulesets.json" >/dev/null; then
  echo 'Reviewed main changes already exists; inspect it before changing protection again' >&2
  exit 1
fi
jq --argjson app_id "$app_id" '{
  name: "Reviewed main changes", target: "branch", enforcement: "active",
  conditions: {ref_name: {include: ["refs/heads/main"], exclude: []}},
  bypass_actors: [{actor_id: $app_id, actor_type: "Integration", bypass_mode: "always"}],
  rules: [
    {type: "pull_request", parameters: {
      required_approving_review_count: .required_pull_request_reviews.required_approving_review_count,
      dismiss_stale_reviews_on_push: .required_pull_request_reviews.dismiss_stale_reviews,
      require_code_owner_review: .required_pull_request_reviews.require_code_owner_reviews,
      require_last_push_approval: .required_pull_request_reviews.require_last_push_approval,
      required_review_thread_resolution: .required_conversation_resolution.enabled
    }},
    {type: "required_status_checks", parameters: {
      strict_required_status_checks_policy: .required_status_checks.strict,
      required_status_checks: [.required_status_checks.checks[] | {
        context: .context, integration_id: .app_id
      }]
    }}
  ]
}' "$temporary/branch.json" >"$temporary/ruleset.json"
gh api --method POST "repos/$repository/rulesets" --input "$temporary/ruleset.json" >"$temporary/created.json"
ruleset_id="$(jq --raw-output '.id' "$temporary/created.json")"
[[ "$ruleset_id" =~ ^[1-9][0-9]*$ ]]
gh api "repos/$repository/rulesets/$ruleset_id" >"$temporary/verified.json"
# Confirm the full active replacement, including bypass identity, before
# removing either duplicate classic requirement. All structural rules remain.
jq --slurpfile expected "$temporary/ruleset.json" --exit-status '
  .enforcement == $expected[0].enforcement and .target == $expected[0].target and
  .conditions == $expected[0].conditions and .bypass_actors == $expected[0].bypass_actors and
  (.rules | length) == ($expected[0].rules | length) and
  (.rules | contains($expected[0].rules))
' "$temporary/verified.json" >/dev/null

gh variable set RELEASE_APP_ID --repo "$repository" --env stable-release --body "$app_id"
gh variable set RELEASE_RULESET_ID --repo "$repository" --env stable-release --body "$ruleset_id"
gh api --method DELETE "repos/$repository/branches/main/protection/required_status_checks"
gh api --method DELETE "repos/$repository/branches/main/protection/required_pull_request_reviews"
# The caller's administrative token also exposes the ruleset bypass list.
export GH_REPOSITORY="$repository" RELEASE_APP_ID="$app_id" RELEASE_RULESET_ID="$ruleset_id"
export GH_TOKEN="${GH_TOKEN:-$(gh auth token)}"
"$(dirname "${BASH_SOURCE[0]}")/check-release-protections.sh"
printf 'Configured single-approval release App %s and ruleset %s.\n' "$app_id" "$ruleset_id"
