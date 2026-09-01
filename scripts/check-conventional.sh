#!/usr/bin/env bash
set -euo pipefail

readonly cog_bin="${COG_BIN:-cog}"
title_file=""

cleanup() {
  if [[ -n "$title_file" ]]; then
    rm -f -- "$title_file"
  fi
}

trap cleanup EXIT

usage() {
  echo "usage: $0 title | range <base-sha> <head-sha>" >&2
  exit 2
}

verify_title() {
  if [[ -z "${PR_TITLE+x}" ]]; then
    echo "PR_TITLE must contain the pull request title" >&2
    exit 2
  fi

  title_file="$(mktemp "${RUNNER_TEMP:-/tmp}/uob-pr-title.XXXXXX")"

  printf '%s\n' "$PR_TITLE" >"$title_file"
  "$cog_bin" verify --file "$title_file"
}

require_commit_sha() {
  local name="$1"
  local value="$2"

  if [[ ! "$value" =~ ^[0-9a-fA-F]{40}$ ]]; then
    echo "$name must be a full 40-character Git commit SHA" >&2
    exit 2
  fi
  git cat-file -e "${value}^{commit}"
}

check_range() {
  local base_sha="$1"
  local head_sha="$2"

  require_commit_sha "base SHA" "$base_sha"
  require_commit_sha "head SHA" "$head_sha"
  local range_base
  range_base="$(git merge-base "$base_sha" "$head_sha")"

  "$cog_bin" check --ignore-merge-commits "${range_base}..${head_sha}"
}

case "${1:-}" in
  title)
    (( $# == 1 )) || usage
    verify_title
    ;;
  range)
    (( $# == 3 )) || usage
    check_range "$2" "$3"
    ;;
  *)
    usage
    ;;
esac
