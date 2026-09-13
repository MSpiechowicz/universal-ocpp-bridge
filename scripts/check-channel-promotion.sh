#!/usr/bin/env bash
set -euo pipefail
: "${PR_HEAD_BRANCH:?PR_HEAD_BRANCH required}"
: "${PR_BASE_BRANCH:?PR_BASE_BRANCH required}"
if [[ "$PR_HEAD_BRANCH" == next && "$PR_BASE_BRANCH" == main ]]; then
  echo 'next-to-main promotion requires the trusted ancestry-preserving channel gate; feature squash is forbidden.' >&2
  exit 1
fi
