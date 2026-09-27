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

fixtures="$repository_root/tests/boundary-fixtures/postgresql-ownership"
"$checker" "$fixtures/allowed"

if output="$("$checker" "$fixtures/rejected-dependencies" 2>&1)"; then
  echo "expected PostgreSQL dependency ownership fixture to fail" >&2
  exit 1
fi
for violation in \
  "postgresql-dependency-violator declares PostgreSQL driver tokio-postgres" \
  "postgresql-dependency-violator declares PostgreSQL driver postgres_rustls" \
  "postgresql-dependency-violator declares prohibited PostgreSQL pool dependency deadpool-postgres" \
  "uob-postgresql-export-adapter declares prohibited PostgreSQL pool dependency bb8" \
  "uob-postgresql-export-adapter declares prohibited PostgreSQL pool dependency deadpool-postgres"; do
  if [[ "$output" != *"$violation"* ]]; then
    echo "PostgreSQL fixture did not reject: $violation" >&2
    echo "$output" >&2
    exit 1
  fi
done

for source in "consumer/src/lib.rs" "consumer/tests/reexport.rs" "consumer/build.rs"; do
  if [[ "$output" != *"references PostgreSQL driver"*"$source"* ]]; then
    echo "PostgreSQL fixture did not reject aliased importer $source" >&2
    echo "$output" >&2
    exit 1
  fi
done

if output="$("$checker" "$fixtures/rejected-sources" 2>&1)"; then
  echo "expected PostgreSQL source ownership fixture to fail" >&2
  exit 1
fi
for source in "consumer/src/lib.rs" "consumer/examples/reexport.rs"; do
  if [[ "$output" != *"$source"* ]]; then
    echo "PostgreSQL fixture did not reject importer $source" >&2
    echo "$output" >&2
    exit 1
  fi
done

echo "boundary rejection fixtures verified"
