#!/usr/bin/env bash
set -euo pipefail

old_ref=${1:-}
new_ref=${2:-}
checkout_type=${3:-0}

if [[ "$checkout_type" != "1" ]]; then
  exit 0
fi

CHANNEL_ARGS=()
channel="${OP_PI_CHANNEL:-}"
if [[ -n "$channel" ]]; then
  CHANNEL_ARGS=(--channel "$channel")
fi

repo=$(basename "$(git rev-parse --show-toplevel)")
old_branch=$(git name-rev --name-only --refs='refs/heads/*' "$old_ref" 2>/dev/null || echo "$old_ref")
new_branch=$(git name-rev --name-only --refs='refs/heads/*' "$new_ref" 2>/dev/null || echo "$new_ref")

exec op_pi git branch-changed \
  --repo "$repo" \
  --old-branch "$old_branch" \
  --new-branch "$new_branch" \
  "${CHANNEL_ARGS[@]}"
