#!/usr/bin/env bash
set -euo pipefail

repository_root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
readonly maximum_lines=500
status=0

while IFS= read -r -d '' file; do
  line_count="$(wc -l < "$file")"
  if (( line_count > maximum_lines )); then
    relative_path="${file#"$repository_root"/}"
    echo "file size violation: $relative_path has $line_count lines (maximum $maximum_lines)" >&2
    status=1
  fi
done < <(
  find "$repository_root" -type f \
    \( -name '*.rs' -o -name '*.sh' -o -name '*.py' -o -name '*.js' -o -name '*.jsx' \
       -o -name '*.ts' -o -name '*.tsx' \) \
    -not -path '*/.git/*' \
    -not -path '*/target/*' \
    -not -path '*/node_modules/*' \
    -not -path '*/vendor/*' \
    -print0
)

if (( status != 0 )); then
  exit "$status"
fi

echo "code file sizes verified (maximum $maximum_lines lines)"
