#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repository_root"

./scripts/check-file-sizes.sh

container_mode=false
if [[ "${1:-}" == "--container" ]]; then
  container_mode=true
fi

if [[ "$container_mode" == true ]]; then
  rustup component add rustfmt clippy
elif ! command -v cargo >/dev/null 2>&1 \
  || ! cargo fmt --version >/dev/null 2>&1 \
  || ! cargo clippy --version >/dev/null 2>&1; then
  if ! command -v docker >/dev/null 2>&1; then
    echo "verification requires Cargo with rustfmt and Clippy, or Docker" >&2
    echo "install Rust with rustup, or install/start Docker and rerun this command" >&2
    exit 127
  fi

  readonly rust_image="rust:1.98.0-bookworm@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922"
  readonly cargo_cache="uob-verify-cargo-home"
  readonly target_cache="uob-verify-target"
  readonly host_uid="$(id -u)"
  readonly host_gid="$(id -g)"

  echo "local Rust toolchain unavailable; verifying with $rust_image"
  set +e
  docker run --rm \
    -e CARGO_HOME=/tmp/cargo-home \
    -e CARGO_TARGET_DIR=/tmp/target \
    -v "$cargo_cache:/tmp/cargo-home" \
    -v "$target_cache:/tmp/target" \
    -v "$repository_root:/workspace" \
    -w /workspace \
    "$rust_image" \
    ./scripts/verify-workspace.sh --container
  verification_status=$?
  set -e

  if [[ -f Cargo.lock ]]; then
    docker run --rm \
      -v "$repository_root:/workspace" \
      "$rust_image" \
      chown "$host_uid:$host_gid" /workspace/Cargo.lock
  fi

  exit "$verification_status"
fi

cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo run --locked --quiet --package uob-ocpp-fixtures
./scripts/test-release-protections.sh
./scripts/test-boundaries.sh

packages=(
  uob-contracts
  uob-database-conformance
  uob-domain
  uob-application
  uob-target-conformance
  uob-ocpp-fixtures
  uob-hostile-websocket-peer
  uob-repository-checks
  uob-protocol-adapter
  uob-target-adapter
  uob-mqtt-target-adapter
  uob-ems-scada-http-target-adapter
  uob-storage-adapter
  uob-external-export-adapter
  uob-provider-adapter
  uob-management-adapter
  uob-service
  uob-sim
  uob-release-manager
)

for package in "${packages[@]}"; do
  cargo check --locked --package "$package"
done

echo "workspace verification complete"
