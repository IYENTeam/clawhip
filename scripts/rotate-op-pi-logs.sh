#!/bin/sh
set -eu

LOG_DIR="${OP_PI_LOG_DIR:-${CLAWHIP_LOG_DIR:-$HOME/.op-pi/logs}}"
if [ ! -d "$LOG_DIR" ] && [ -d "$HOME/.clawhip/logs" ]; then
    LOG_DIR="$HOME/.clawhip/logs"
fi
MAX_BYTES="${OP_PI_LOG_MAX_BYTES:-${CLAWHIP_LOG_MAX_BYTES:-26214400}}"
KEEP="${OP_PI_LOG_KEEP:-${CLAWHIP_LOG_KEEP:-4}}"
SERVICE_LABEL="${OP_PI_SERVICE_LABEL:-${CLAWHIP_SERVICE_LABEL:-com.op-pi.daemon}}"
LAUNCHD_DOMAIN="${OP_PI_LAUNCHD_DOMAIN:-${CLAWHIP_LAUNCHD_DOMAIN:-gui/$(id -u)}}"
ROTATED_FILES=""

case "$MAX_BYTES:$KEEP" in
    *[!0-9:]*|:*|*:) echo "invalid numeric log rotation setting" >&2; exit 64 ;;
esac

rotate_log() {
    file="$1"
    [ -f "$file" ] || return 0
    size="$(stat -f %z "$file")"
    [ "$size" -gt "$MAX_BYTES" ] || return 0

    rm -f "$file.$KEEP.gz"
    slot=$((KEEP - 1))
    while [ "$slot" -ge 1 ]; do
        if [ -f "$file.$slot.gz" ]; then
            mv "$file.$slot.gz" "$file.$((slot + 1)).gz"
        fi
        slot=$((slot - 1))
    done

    mv "$file" "$file.1"
    : > "$file"
    ROTATED_FILES="${ROTATED_FILES}${file}.1
"
}

mkdir -p "$LOG_DIR"
rotate_log "$LOG_DIR/stderr.log"
rotate_log "$LOG_DIR/stdout.log"

[ -n "$ROTATED_FILES" ] || exit 0

if [ "${OP_PI_LOG_ROTATE_SKIP_RESTART:-${CLAWHIP_LOG_ROTATE_SKIP_RESTART:-0}}" != "1" ]; then
    launchctl kickstart -k "$LAUNCHD_DOMAIN/$SERVICE_LABEL"
fi

printf '%s' "$ROTATED_FILES" | while IFS= read -r rotated; do
    [ -n "$rotated" ] && gzip -f "$rotated"
done
