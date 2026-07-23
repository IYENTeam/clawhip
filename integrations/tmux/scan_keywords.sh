#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: scan_keywords.sh --session <session> --keywords <comma,list> [--channel <id>]" >&2
  exit 1
}

session=""
keywords=""
channel="${OP_PI_CHANNEL:-}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --session) session="$2"; shift 2 ;;
    --keywords) keywords="$2"; shift 2 ;;
    --channel) channel="$2"; shift 2 ;;
    *) usage ;;
  esac
done

[[ -n "$session" && -n "$keywords" ]] || usage

state_root="${XDG_STATE_HOME:-$HOME/.local/state}"
state_dir="$state_root/op_pi/tmux_keywords"
mkdir -p "$state_dir"
patterns=(${keywords//,/ })

while IFS='|' read -r pane_id pane_name; do
  [[ -n "$pane_id" ]] || continue
  pane_output=$(tmux capture-pane -p -t "$pane_id" -S -200)
  state_file="$state_dir/${session}_${pane_id//[%:]/_}.txt"
  previous_output=""
  if [[ -f "$state_file" ]]; then
    previous_output=$(cat "$state_file")
  fi
  printf '%s' "$pane_output" > "$state_file"

  while IFS= read -r line; do
    [[ "$previous_output" == *"$line"* ]] && continue
    for keyword in "${patterns[@]}"; do
      if [[ "$line" == *"$keyword"* ]]; then
        args=(op_pi tmux keyword --session "$session" --keyword "$keyword" --line "$line")
        if [[ -n "$channel" ]]; then args+=(--channel "$channel"); fi
        "${args[@]}"
      fi
    done
  done <<< "$pane_output"
done < <(tmux list-panes -t "$session" -F '#{pane_id}|#{window_index}.#{pane_index}')
