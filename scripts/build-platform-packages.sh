#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
case "$(uname -m)" in
  x86_64) target=x86_64-unknown-linux-gnu ;;
  aarch64) target=aarch64-unknown-linux-gnu ;;
  *) echo 'unsupported native package architecture' >&2; exit 1 ;;
esac
readonly image='rust:1.98.0-bookworm@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922'
mkdir -p target/package-build target/package-cargo
# Build tools remain on the builder. Only audited runtime inputs enter the archives.
docker run --rm --user "$(id -u):$(id -g)" \
  -v "$PWD:/workspace" -w /workspace \
  -e CARGO_HOME=/workspace/target/package-cargo \
  -e CARGO_TARGET_DIR=/workspace/target/package-build \
  "$image" sh -ec '
    cargo build --locked --release -p uob-service --bin uob -p uob-sim --bin uob-sim
    rustc -Vv > target/package-build/rustc.txt
  '
python3 -B scripts/platform_packages.py --target "$target" \
  --binary-dir target/package-build --output "${1:-target/platform-packages}"
