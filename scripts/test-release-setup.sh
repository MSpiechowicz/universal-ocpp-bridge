#!/usr/bin/env bash
set -euo pipefail
readonly root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly temporary="$(mktemp -d)"
trap 'rm -rf -- "$temporary"' EXIT
mkdir "$temporary/bin" "$temporary/scripts"
cp "$root/scripts/configure-release-protections.sh" "$temporary/scripts/"
cat > "$temporary/scripts/check-release-protections.sh" <<'CHECK'
#!/usr/bin/env bash
[[ "$RELEASE_APP_ID" == 12345 && "$RELEASE_RULESET_ID" == 42 ]]
CHECK
chmod +x "$temporary/scripts/check-release-protections.sh"
export RELEASE_SETUP_TEST="$temporary" GH_REPO=example/repository GH_TOKEN=fixture
cat > "$temporary/branch.json" <<'JSON'
{
  "required_pull_request_reviews": {
    "required_approving_review_count": 1,
    "dismiss_stale_reviews": true,
    "require_code_owner_reviews": true,
    "require_last_push_approval": true
  },
  "required_status_checks": {
    "strict": true,
    "checks": [{"context": "CI", "app_id": 15368}]
  },
  "required_conversation_resolution": {"enabled": true}
}
JSON
cat > "$temporary/bin/gh" <<'GH'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  'api repos/example/repository/branches/main/protection') cat "$RELEASE_SETUP_TEST/branch.json" ;;
  'api repos/example/repository/rulesets?per_page=100') echo '[]' ;;
  'api --method POST repos/example/repository/rulesets --input '*)
    jq '. + {id: 42}' "${@: -1}" >"$RELEASE_SETUP_TEST/created.json"
    cat "$RELEASE_SETUP_TEST/created.json" ;;
  'api repos/example/repository/rulesets/42')
    if [[ -f "$RELEASE_SETUP_TEST/invalid" ]]; then
      jq '.enforcement = "disabled"' "$RELEASE_SETUP_TEST/created.json"
    else
      cat "$RELEASE_SETUP_TEST/created.json"
    fi ;;
  'variable set RELEASE_APP_ID --repo example/repository --env stable-release --body 12345'|\
  'variable set RELEASE_RULESET_ID --repo example/repository --env stable-release --body 42'|\
  'api --method DELETE repos/example/repository/branches/main/protection/required_status_checks'|\
  'api --method DELETE repos/example/repository/branches/main/protection/required_pull_request_reviews')
    echo "$*" >>"$RELEASE_SETUP_TEST/mutations" ;;
  *) echo "unexpected gh command: $*" >&2; exit 1 ;;
esac
GH
chmod +x "$temporary/bin/gh"
export PATH="$temporary/bin:$PATH"
"$temporary/scripts/configure-release-protections.sh" 12345
jq --exit-status '
  .bypass_actors == [{actor_id: 12345, actor_type: "Integration", bypass_mode: "always"}] and
  any(.rules[]; .type == "pull_request" and .parameters.required_approving_review_count == 1 and
      .parameters.require_code_owner_review == true and .parameters.require_last_push_approval == true) and
  any(.rules[]; .type == "required_status_checks" and .parameters.required_status_checks == [{context: "CI", integration_id: 15368}])
' "$temporary/created.json" >/dev/null
test "$(wc -l < "$temporary/mutations")" = 4
rm "$temporary/mutations"
touch "$temporary/invalid"
if "$temporary/scripts/configure-release-protections.sh" 12345; then
  echo 'accepted inactive replacement ruleset' >&2; exit 1
fi
test ! -f "$temporary/mutations"
echo 'release setup preserves review/check settings and verifies before removing duplicates'
