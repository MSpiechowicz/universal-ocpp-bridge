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
  # Recover only the generated direct child of this exact approved source.
  # A later unrelated main run owns its own publication.
  if [[ "$(git rev-parse origin/main^)" == "$source_revision" ]] &&
     [[ "$(git log -1 --format='%(trailers:key=Release-Source,valueonly)' origin/main)" == "$source_revision" ]]; then
    python3 scripts/verify-release-diff.py "$source_revision" origin/main
    git switch --detach origin/main
  else
    echo 'Skipped superseded main revision; the newer main run owns publication.' >>"$summary"
    exit 0
  fi
fi
readonly publication_base="$(git rev-parse HEAD)"

version="$(cargo metadata --locked --no-deps --format-version 1 | jq --raw-output '.packages[] | select(.name == "uob-service") | .version')"
[[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]
tag="v$version"
if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then
  tag_revision="$(git rev-parse "$tag^{commit}")"
  if [[ "$tag_revision" != "$publication_base" ]]; then
    git merge-base --is-ancestor "$tag_revision" "$publication_base" || {
      echo "Refusing conflicting release tag $tag" >&2
      exit 1
    }
    # One approved job owns both the version commit and publication.
    git switch -C main "$source_revision"
    cog bump --auto --skip-ci
    if [[ "$(git rev-parse HEAD)" == "$source_revision" ]]; then
      echo 'No release-eligible changes since the current version.' >>"$summary"
      exit 0
    fi
    tag="$(git tag --points-at HEAD --list 'v[0-9]*')"
    [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]
    # Keep an explicit source marker so rerunning an approved job can recover
    # an API failure after the atomic main/tag push.
    git commit --amend --no-edit --trailer "Release-Source: $source_revision"
    git tag --force "$tag" HEAD
    python3 scripts/verify-release-diff.py "$source_revision" HEAD
    cargo check --locked --workspace --all-targets
    git diff --exit-code HEAD
    # Non-fast-forward rejection protects a newer main; atomic push ensures
    # main and its version tag either both advance or neither does.
    git push --atomic origin HEAD:refs/heads/main "refs/tags/$tag"
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
  cargo check --locked --workspace --all-targets
  git fetch origin main
  if [[ "$(git rev-parse origin/main)" != "$source_revision" ]]; then
    echo 'Skipped superseded main revision after verification.' >>"$summary"
    exit 0
  fi
  git tag "$tag" "$source_revision"
  # Publish only the tag on the unchanged, verified main revision.
  git push origin "refs/tags/$tag"
fi

# A retry after a successful tag push must recover a failed Release API request.
releases="$(gh release list --limit 100 --json tagName)"
if ! jq --exit-status --arg tag "$tag" 'any(.[]; .tagName == $tag)' <<<"$releases" >/dev/null; then
  gh release create "$tag" --verify-tag --generate-notes --latest --title "$tag"
fi
printf 'Published %s from verified main revision %s.\n' "$tag" "$(git rev-parse HEAD)" >>"$summary"
