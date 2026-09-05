#!/usr/bin/env bash
# Run on a disposable Linux runner/container, never on a charging host.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
manifest="$(mktemp)"
unit_directory="$(mktemp -d)"
trap 'rm -f "$manifest"; rm -rf "$unit_directory"' EXIT
cargo test --locked -p uob-service --test deployment --no-run --message-format=json >"$manifest"
executable="$(jq -ser '[.[] | select(.reason == "compiler-artifact" and .target.name == "deployment" and .executable != null)] | if length == 1 then .[0].executable else error("expected one deployment test executable") end' "$manifest")"
service_binary="$(dirname "$(dirname "$executable")")/uob"
# Verify the shipped directives with the just-built executable; install nothing on the host.
for unit in packaging/systemd/*.service; do
  sed "s|/usr/local/bin/uob|$service_binary|g" "$unit" >"$unit_directory/$(basename "$unit")"
done
cp packaging/systemd/*.slice "$unit_directory/"
systemd-analyze verify "$unit_directory"/*.service "$unit_directory"/*.slice
if [[ "$EUID" == 0 ]]; then
  "$executable" --ignored --exact distinct_linux_users_cannot_write_peer_state_or_sockets
else
  sudo -- "$executable" --ignored --exact distinct_linux_users_cannot_write_peer_state_or_sockets
fi
