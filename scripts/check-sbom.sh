#!/usr/bin/env bash
set -euo pipefail

readonly sbom_path="${1:?usage: check-sbom.sh SBOM_PATH}"
readonly source_revision="${SOURCE_REVISION:?SOURCE_REVISION is required}"

jq --exit-status --arg revision "$source_revision" '
  .bomFormat == "CycloneDX"
  and .metadata.component.name == "universal-ocpp-bridge"
  and .metadata.component.version == $revision
' "$sbom_path" >/dev/null

while IFS=$'\t' read -r package version; do
  jq --exit-status --arg package "$package" --arg version "$version" '
    any(.components[]?; .name == $package and .version == $version)
  ' "$sbom_path" >/dev/null || {
    echo "SBOM missing locked dependency $package@$version" >&2
    exit 1
  }
done < <(
  awk '
    /^name = / { name = $3; gsub(/"/, "", name) }
    /^version = / { version = $3; gsub(/"/, "", version) }
    /^source = / && name != "" && version != "" {
      print name "\t" version
      name = ""
      version = ""
    }
  ' Cargo.lock | sort -u
)

echo "SBOM identifies $source_revision and every locked registry dependency"
