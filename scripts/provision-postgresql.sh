#!/usr/bin/env bash
# Provision one PostgreSQL export schema using the libpq PG* environment or service.
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 install|upgrade SCHEMA RUNTIME_ROLE" >&2
  exit 64
fi

mode=$1
schema_name=$2
runtime_role=$3
case "$mode" in
  install|upgrade) ;;
  *)
    echo "mode must be install or upgrade" >&2
    exit 64
    ;;
esac
if [[ -z "$schema_name" || -z "$runtime_role" ]]; then
  echo "schema and runtime role must not be empty" >&2
  exit 64
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec psql -X -v ON_ERROR_STOP=1 \
  -v "provision_mode=$mode" \
  -v "schema_name=$schema_name" \
  -v "runtime_role=$runtime_role" \
  -v target_version=2 \
  --file "$script_dir/../packaging/postgresql/export/provision.sql"
