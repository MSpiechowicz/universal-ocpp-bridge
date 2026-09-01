#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly checker="$repository_root/scripts/check-conventional.sh"
readonly workflow="$repository_root/.github/workflows/conventional.yml"
readonly cog_bin="${COG_BIN:-cog}"

if ! command -v "$cog_bin" >/dev/null 2>&1; then
  echo "Cocogitto is required; install the pinned version documented in docs/contributing.md" >&2
  exit 127
fi

for title in \
  "feat(api): add a command endpoint" \
  "fix(ocpp): reject a duplicate identifier" \
  "docs: explain local validation" \
  "feat(api)!: change command semantics"
do
  PR_TITLE="$title" COG_BIN="$cog_bin" "$checker" title >/dev/null
done

for invalid_title in "malformed title" "Merge branch 'topic'"; do
  if PR_TITLE="$invalid_title" COG_BIN="$cog_bin" "$checker" title >/dev/null 2>&1; then
    echo "a malformed pull request title unexpectedly passed: $invalid_title" >&2
    exit 1
  fi
done

test_root="$(mktemp -d "${RUNNER_TEMP:-/tmp}/uob-conventional-test.XXXXXX")"
trap 'rm -rf -- "$test_root"' EXIT
marker="$test_root/title-was-executed"
malicious_title="docs: preserve \$(touch $marker) as literal text"
PR_TITLE="$malicious_title" COG_BIN="$cog_bin" "$checker" title >/dev/null
if [[ -e "$marker" ]]; then
  echo "pull request title content was executed by the shell" >&2
  exit 1
fi

history_repo="$test_root/history"
git init --quiet --initial-branch=main "$history_repo"
cp "$repository_root/cog.toml" "$history_repo/cog.toml"
git -C "$history_repo" config user.name "Conventional Check Test"
git -C "$history_repo" config user.email "conventional-check@example.invalid"
git -C "$history_repo" commit --quiet --allow-empty --message "legacy non-conventional history"
git -C "$history_repo" commit --quiet --allow-empty --message "chore: adopt Conventional Commits"
baseline_sha="$(git -C "$history_repo" rev-parse HEAD)"
git -C "$history_repo" switch --quiet --create topic
git -C "$history_repo" commit --quiet --allow-empty --message "docs: document topic"
git -C "$history_repo" switch --quiet main
git -C "$history_repo" commit --quiet --allow-empty --message "fix: stabilize main"
git -C "$history_repo" merge --quiet --no-ff topic --message "Merge branch 'topic'"
head_sha="$(git -C "$history_repo" rev-parse HEAD)"
(
  cd "$history_repo"
  COG_BIN="$cog_bin" "$checker" range "$baseline_sha" "$head_sha" >/dev/null
)

git -C "$history_repo" commit --quiet --allow-empty --message "invalid new commit"
invalid_head_sha="$(git -C "$history_repo" rev-parse HEAD)"
if (
  cd "$history_repo"
  COG_BIN="$cog_bin" "$checker" range "$baseline_sha" "$invalid_head_sha" >/dev/null 2>&1
); then
  echo "a malformed commit in the checked range unexpectedly passed" >&2
  exit 1
fi

grep -Fq "types: [opened, synchronize, edited, reopened]" "$workflow"
grep -Fq "contents: read" "$workflow"

echo "Conventional title and commit-range checks verified"
