#!/usr/bin/env bash
set -euo pipefail

# Calculate only: no fetch, checkout, hooks, manifest edits, commits, tags or publication.
if (( $# > 1 )); then
  echo "usage: $0 [repository]" >&2
  exit 2
fi
readonly cog_bin="${COG_BIN:-cog}"
readonly repository="${1:-.}"
trap 'echo "Product version calculation failed; this is not a no-release result." >&2' ERR
cd "$repository"
tool_version="$("$cog_bin" --version)"
test "$tool_version" = 'cog 7.0.0'
test -f cog.toml
shallow="$(git rev-parse --is-shallow-repository)"
test "$shallow" = false
working_changes="$(git status --porcelain --untracked-files=normal)"
test -z "$working_changes"
source_revision="$(git rev-parse HEAD)"
readonly source_revision
readonly stable_pattern='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'

# Require a real established baseline instead of accepting cog's default initial version.
# Run the calculator even without a baseline so invalid config/history still fails distinctly.
proposal="$("$cog_bin" bump --auto --dry-run)"
readonly no_release=$'No conventional commits for your repository that required a bump. Changelogs will be updated on the next bump.\nPre-Hooks and Post-Hooks have been skipped.'
if [[ "$proposal" != "$no_release" && ! "$proposal" =~ $stable_pattern ]]; then
  echo 'Unexpected Cocogitto proposal output (stable version policy only).' >&2
  exit 1
fi

reachable_tags="$(git tag --merged HEAD --list 'v*')"
has_baseline=false
while IFS= read -r tag; do
  if [[ "$tag" =~ $stable_pattern ]]; then has_baseline=true; fi
done <<<"$reachable_tags"
baseline=""
status=readiness_required
reason=first_stable_decision_required
if [[ "$has_baseline" == true ]]; then
  baseline="$("$cog_bin" get-version --tag)"
  [[ "$baseline" =~ $stable_pattern ]]
  baseline_revision="$(git rev-parse "refs/tags/$baseline^{commit}")"
  # 7.0.0 can include a release-eligible HEAD again when it is itself the tag target.
  # Still require the calculator above to succeed; a tag must never hide a config error.
  if [[ "$baseline_revision" == "$source_revision" || "$proposal" == "$no_release" ]]; then
    status=no_release
    reason=no_eligible_changes
    proposal=""
  elif [[ "$baseline" == v0.* ]]; then
    # Cocogitto keeps breaking changes within 0.x; neither result establishes stable readiness.
    status=internal_candidate
    reason=first_stable_decision_required
  else
    status=proposed
    reason=eligible_changes
  fi
else
  proposal=""
fi

jq --null-input --compact-output \
  --arg status "$status" --arg reason "$reason" \
  --arg baseline "${baseline#v}" --arg proposed_version "${proposal#v}" \
  --arg source_revision "$source_revision" \
  '{status: $status, reason: $reason,
    baseline: (if $baseline == "" then null else $baseline end),
    proposed_version: (if $proposed_version == "" then null else $proposed_version end),
    source_revision: $source_revision, publication_authorized: false}'
