#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
checker="$repository_root/scripts/check-boundaries.sh"

"$checker" "$repository_root"

for fixture in dependency-violation source-violation; do
  if "$checker" "$repository_root/tests/boundary-fixtures/$fixture" >/dev/null 2>&1; then
    echo "expected boundary fixture $fixture to fail" >&2
    exit 1
  fi
done

echo "boundary rejection fixtures verified"
