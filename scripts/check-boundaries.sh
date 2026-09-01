#!/usr/bin/env bash
set -euo pipefail

repository_root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
protected_packages=(
  "crates/contracts"
  "crates/domain"
  "crates/application"
)

dependencies_in() {
  awk '
    /^\[(dev-|build-)?dependencies\]$/ { in_dependencies = 1; next }
    /^\[/ { in_dependencies = 0 }
    in_dependencies { print }
  ' "$1" | sed -n 's/^[[:space:]]*\([A-Za-z0-9_-]*\)[[:space:]]*=.*$/\1/p'
}

for package in "${protected_packages[@]}"; do
  package_root="$repository_root/$package"
  [[ -d "$package_root" ]] || continue

  manifest="$package_root/Cargo.toml"
  while IFS= read -r dependency; do
    case "$package:$dependency" in
      crates/contracts:jsonschema | crates/contracts:schemars | crates/contracts:serde | crates/contracts:serde_json | crates/contracts:time) ;;
      crates/domain:uob-contracts) ;;
      crates/application:uob-contracts | crates/application:uob-domain) ;;
      *)
        echo "boundary violation: protected package $package declares forbidden dependency $dependency" >&2
        exit 1
        ;;
    esac
  done < <(dependencies_in "$manifest")

  if grep -REin --include='*.rs' '(rust_?ocpp|mqtt|topic|node_?id|namespace_?uri|register_?address|opcua|rumqttc|rusqlite|postgres|axum|tokio::net|websocket|http::|react|vite)' "$package_root/src"; then
    echo "boundary violation: protected package $package contains adapter, transport, industrial mapping, database, or UI details" >&2
    exit 1
  fi
done

if command -v cargo >/dev/null 2>&1 && [[ -f "$repository_root/Cargo.toml" ]]; then
  service_graph="$(cd "$repository_root" && cargo tree -p uob-service --prefix none)"
  if grep -Eiq '(uob-sim|uob-release-manager|ocpp-client)' <<<"$service_graph"; then
    echo "boundary violation: production service graph contains simulator or release-manager code" >&2
    exit 1
  fi
fi

echo "dependency boundaries verified"
