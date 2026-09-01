#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <semver>" >&2
  exit 2
fi

readonly requested_version="$1"
if [[ ! "$requested_version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid semantic version: $requested_version" >&2
  exit 2
fi

readonly repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly manifest="$repository_root/Cargo.toml"
temporary_manifest="$(mktemp "$repository_root/.Cargo.toml.version.XXXXXX")"
trap 'rm -f "$temporary_manifest"' EXIT

awk -v requested_version="$requested_version" '
  BEGIN {
    in_workspace_package = 0
    replacements = 0
  }
  $0 == "[workspace.package]" {
    in_workspace_package = 1
    print
    next
  }
  in_workspace_package && /^\[/ {
    in_workspace_package = 0
  }
  in_workspace_package && /^[[:space:]]*version[[:space:]]*=/ {
    print "version = \"" requested_version "\""
    replacements += 1
    next
  }
  { print }
  END {
    if (replacements != 1) {
      exit 42
    }
  }
' "$manifest" >"$temporary_manifest" || {
  echo "expected exactly one workspace package version in $manifest" >&2
  exit 1
}

chmod --reference="$manifest" "$temporary_manifest"
mv "$temporary_manifest" "$manifest"

cd "$repository_root"
cargo update --package uob-service --precise "$requested_version" --offline
cargo metadata --locked --no-deps --format-version 1 >/dev/null
