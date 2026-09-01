#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repository_root"

cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
./scripts/test-boundaries.sh

packages=(
  uob-contracts
  uob-domain
  uob-application
  uob-protocol-adapter
  uob-target-adapter
  uob-storage-adapter
  uob-external-export-adapter
  uob-provider-adapter
  uob-management-adapter
  uob-service
  uob-sim
  uob-release-manager
)

for package in "${packages[@]}"; do
  cargo check --package "$package"
done

echo "workspace verification complete"
