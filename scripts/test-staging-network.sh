#!/usr/bin/env bash
# Disposable Linux only. All named namespaces/mounts are private to this harness.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if [[ "${1:-}" != --isolated ]]; then
  manifest="$(mktemp)"
  trap 'rm -f "$manifest"' EXIT
  cargo test --locked -p uob-service --test staging_network --test deployment \
    --no-run --message-format=json >"$manifest"
  network_test="$(jq -ser '[.[] | select(.reason == "compiler-artifact" and .target.name == "staging_network" and .executable != null)] | if length == 1 then .[0].executable else error("expected one network test") end' "$manifest")"
  deployment_test="$(jq -ser '[.[] | select(.reason == "compiler-artifact" and .target.name == "deployment" and .executable != null)] | if length == 1 then .[0].executable else error("expected one deployment test") end' "$manifest")"
  if [[ "$EUID" == 0 ]]; then
    unshare --mount --propagation private "$0" --isolated "$network_test" "$deployment_test"
  else
    sudo -- unshare --mount --propagation private "$0" --isolated "$network_test" "$deployment_test"
  fi
  exit
fi
# Hide the host's named namespaces without creating/modifying any host /run directories.
mount -t tmpfs -o mode=0755 tmpfs /run
mkdir /run/netns
packaging/network/uob-staging-network start
trap 'packaging/network/uob-staging-network stop' EXIT
if packaging/network/uob-staging-network start; then
  echo 'existing namespace was incorrectly adopted' >&2
  exit 1
fi
"$2" --ignored --exact isolated_peers_work_and_host_sockets_are_unreachable
# Filesystem assertions run independently of networking; both fixture users are inside the
# disposable namespace so staging's runtime proof remains mandatory in this older harness.
ip netns exec uob-staging "$3" --ignored --exact distinct_linux_users_cannot_write_peer_state_or_sockets
