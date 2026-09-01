#!/usr/bin/env bash
set -euo pipefail

repository_root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$workspace_root"
cargo run --quiet --locked --package uob-repository-checks -- "$repository_root"
