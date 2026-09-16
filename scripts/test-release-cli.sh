#!/usr/bin/env bash
set -euo pipefail
repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repository_root"
cargo build --locked --package uob-service --bin uob
target_directory="$(cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
export UOB_TEST_CLI="$target_directory/debug/uob"
cargo test --locked --package uob-release-manager --test supervisor cli:: -- --ignored
cargo test --locked --package uob-release-manager --test production_activation \
  audit::cli_reads_decisions_after_rollback_and_bridge_crash -- --ignored
