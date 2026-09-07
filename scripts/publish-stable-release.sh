#!/usr/bin/env bash
set -euo pipefail

# Run only after workspace verification and the live protection check.
readonly source_revision="$(git rev-parse HEAD)"
readonly summary="${GITHUB_STEP_SUMMARY:-/dev/stdout}"
readonly repository="${GH_REPO:?GH_REPO is required}"
git diff --exit-code HEAD
# Never move a newer main back to the source of an older queued workflow.
git fetch origin main --tags
if [[ "$(git rev-parse origin/main)" != "$source_revision" ]]; then
  echo 'Skipped superseded main revision; the newer main run owns publication.' >>"$summary"
  exit 0
fi

version="$(cargo metadata --locked --no-deps --format-version 1 | jq --raw-output '.packages[] | select(.name == "uob-service") | .version')"
[[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]
readonly tag="v$version"
if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then
  tag_revision="$(git rev-parse "$tag^{commit}")"
  if [[ "$tag_revision" != "$source_revision" ]]; then
    git merge-base --is-ancestor "$tag_revision" "$source_revision" || {
      echo "Refusing conflicting release tag $tag" >&2
      exit 1
    }
    # The current version is already released. Prepare the next one for review.
    # Cocogitto requires main locally; this does not change the remote branch.
    git switch -C main "$source_revision"
    cog bump --auto
    if [[ "$(git rev-parse HEAD)" == "$source_revision" ]]; then
      echo 'No release-eligible changes since the current version.' >>"$summary"
      exit 0
    fi
    next_tag="$(git tag --points-at HEAD --list 'v[0-9]*')"
    [[ "$next_tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]
    cargo check --locked --package uob-service
    release_branch="codex/release-${next_tag}-${source_revision}"
    # On retries keep the already prepared immutable branch, never force-push it.
    remote_branch="$(git ls-remote --heads origin "refs/heads/$release_branch")"
    if [[ -z "$remote_branch" ]]; then
      git push origin "HEAD:refs/heads/$release_branch"
    fi
    printf 'Version %s is prepared. Open and merge its PR after required checks pass:\n\n%s/%s/compare/main...%s?expand=1\n' \
      "$next_tag" "${GITHUB_SERVER_URL:-https://github.com}" "$repository" "$release_branch" >>"$summary"
    exit 0
  fi
else
  # The reviewed main commit contains an unreleased workspace version.
  latest_tag="$(git tag --merged HEAD --list 'v[0-9]*' --sort=-version:refname | head -n 1)"
  python3 - "$version" "${latest_tag#v}" <<'PY'
import sys

def version(value):
    return tuple(map(int, value.split('.')))

if sys.argv[2] and version(sys.argv[1]) <= version(sys.argv[2]):
    sys.exit('Refusing a non-increasing release version')
PY
  cargo check --locked --package uob-service
  git tag "$tag" "$source_revision"
  # Publish only the tag on the unchanged, verified main revision.
  git push origin "refs/tags/$tag"
fi

# A retry after a successful tag push must recover a failed Release API request.
releases="$(gh release list --limit 100 --json tagName)"
if ! jq --exit-status --arg tag "$tag" 'any(.[]; .tagName == $tag)' <<<"$releases" >/dev/null; then
  gh release create "$tag" --verify-tag --generate-notes --latest --title "$tag"
fi
printf 'Published %s from verified main revision %s.\n' "$tag" "$source_revision" >>"$summary"
