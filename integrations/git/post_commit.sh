#!/usr/bin/env bash
set -euo pipefail

CHANNEL_ARGS=()
channel="${OP_PI_CHANNEL:-}"
if [[ -n "$channel" ]]; then
  CHANNEL_ARGS=(--channel "$channel")
fi

repo=$(basename "$(git rev-parse --show-toplevel)")
branch=$(git rev-parse --abbrev-ref HEAD)
commit=$(git rev-parse HEAD)
summary=$(git log -1 --pretty=%s)

exec op_pi git commit \
  --repo "$repo" \
  --branch "$branch" \
  --commit "$commit" \
  --summary "$summary" \
  "${CHANNEL_ARGS[@]}"
