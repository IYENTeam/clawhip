#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: stale_check.sh --session <session> --stale-minutes <n> [--channel <id>]" >&2
  exit 1
}

session=""
stale_minutes=""
channel="${OP_PI_CHANNEL:-}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --session) session="$2"; shift 2 ;;
    --stale-minutes) stale_minutes="$2"; shift 2 ;;
    --channel) channel="$2"; shift 2 ;;
    *) usage ;;
  esac
done

[[ -n "$session" && -n "$stale_minutes" ]] || usage

state_root="${XDG_STATE_HOME:-$HOME/.local/state}"
state_dir="$state_root/op_pi/tmux_stale"
mkdir -p "$state_dir"
now=$(date +%s)
threshold=$((stale_minutes * 60))

while IFS='|' read -r pane_id pane_name; do
  [[ -n "$pane_id" ]] || continue
  pane_output=$(tmux capture-pane -p -t "$pane_id" -S -200)
  last_line=$(printf '%s\n' "$pane_output" | awk 'NF { line=$0 } END { print line }')
  hash=$(printf '%s' "$pane_output" | sha1sum | awk '{print $1}')
  state_file="$state_dir/${session}_${pane_id//[%:]/_}.state"

  prev_hash=""
  changed_at=$now
  notified_at=0
  if [[ -f "$state_file" ]]; then
    source "$state_file"
  fi

  if [[ "$hash" != "$prev_hash" ]]; then
    changed_at=$now
    notified_at=0
  elif (( now - changed_at >= threshold )) && (( notified_at == 0 || now - notified_at >= threshold )); then
    args=(op_pi tmux stale --session "$session" --pane "$pane_name" --minutes "$stale_minutes" --last-line "${last_line:-<no output>}")
    if [[ -n "$channel" ]]; then args+=(--channel "$channel"); fi
    "${args[@]}"
    notified_at=$now
  fi

  cat > "$state_file" <<STATE
prev_hash=$hash
changed_at=$changed_at
notified_at=$notified_at
STATE
done < <(tmux list-panes -t "$session" -F '#{pane_id}|#{window_index}.#{pane_index}')
