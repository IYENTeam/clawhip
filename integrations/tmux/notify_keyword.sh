#!/usr/bin/env bash
set -euo pipefail

session=${1:?usage: notify_keyword.sh <session> <keyword> <line> [channel]}
keyword=${2:?usage: notify_keyword.sh <session> <keyword> <line> [channel]}
line=${3:?usage: notify_keyword.sh <session> <keyword> <line> [channel]}
channel=${4:-${OP_PI_CHANNEL:-}}

CHANNEL_ARGS=()
if [[ -n "$channel" ]]; then
  CHANNEL_ARGS=(--channel "$channel")
fi

exec op_pi tmux keyword \
  --session "$session" \
  --keyword "$keyword" \
  --line "$line" \
  "${CHANNEL_ARGS[@]}"
