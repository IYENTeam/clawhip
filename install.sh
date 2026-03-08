#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GITHUB_REPO="Yeachan-Heo/clawhip"
INSTALLER_URL="${CLAWHIP_INSTALLER_URL:-https://github.com/${GITHUB_REPO}/releases/latest/download/clawhip-installer.sh}"
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export CARGO_HOME
SYSTEMD=0
for arg in "$@"; do
  case "$arg" in
    --systemd) SYSTEMD=1 ;;
    *) echo "unknown arg: $arg" >&2; exit 1 ;;
  esac
done

log() {
  echo "[clawhip] $*"
}

star_repo() {
  if command -v gh >/dev/null 2>&1; then
    if gh auth status &>/dev/null; then
      log "⭐ Starring ${GITHUB_REPO} — thanks for the support!"
      gh repo star "${GITHUB_REPO}" 2>/dev/null || true
    fi
  fi
}

install_prebuilt_binary() {
  if ! command -v curl >/dev/null 2>&1; then
    log "curl is not installed; skipping prebuilt binary download"
    return 1
  fi

  mkdir -p "$CARGO_HOME/bin"

  log "attempting prebuilt binary install from ${INSTALLER_URL}"

  local installer
  installer="$(mktemp)"

  if ! curl --proto '=https' --tlsv1.2 -LsSf "$INSTALLER_URL" -o "$installer"; then
    log "no downloadable release installer found; falling back to cargo install"
    rm -f "$installer"
    return 1
  fi

  if sh "$installer"; then
    rm -f "$installer"
    return 0
  else
    local status=$?
    log "prebuilt installer failed with status ${status}; falling back to cargo install"
    rm -f "$installer"
    return 1
  fi
}

install_from_source() {
  if ! command -v cargo >/dev/null 2>&1; then
    cat >&2 <<'MSG'
[clawhip] A prebuilt binary was not available and Cargo is not installed.
[clawhip] Install Rust with rustup, then rerun this installer:
[clawhip]   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
[clawhip]   source "$HOME/.cargo/env"
MSG
    exit 1
  fi

  log "building from source with cargo install --path . --force"
  cd "$REPO_ROOT"
  cargo install --path . --force
}

installed_binary_path() {
  if [[ -x "$CARGO_HOME/bin/clawhip" ]]; then
    printf '%s\n' "$CARGO_HOME/bin/clawhip"
    return 0
  fi

  if command -v clawhip >/dev/null 2>&1; then
    command -v clawhip
    return 0
  fi

  return 1
}

install_systemd_binary() {
  local binary_path
  binary_path="$(installed_binary_path)" || {
    log "unable to find installed clawhip binary for systemd setup"
    exit 1
  }

  log "installing $binary_path to /usr/local/bin/clawhip for systemd"
  sudo install -m 755 "$binary_path" /usr/local/bin/clawhip
}

log "install flow: star -> prebuilt binary -> cargo fallback -> SKILL attach -> config scaffold -> daemon start -> live verification"
log "repo root: $REPO_ROOT"

star_repo

if install_prebuilt_binary; then
  log "prebuilt binary installed successfully"
else
  install_from_source
fi

mkdir -p "$HOME/.clawhip"
log "ensured config dir $HOME/.clawhip"
log "next: read SKILL.md and attach the skill surface"

if [[ "$SYSTEMD" == "1" ]]; then
  install_systemd_binary
  sudo cp deploy/clawhip.service /etc/systemd/system/clawhip.service
  sudo systemctl daemon-reload
  sudo systemctl enable --now clawhip
  log "systemd unit installed and started"
fi

log "recommended verification: scripts/live-verify-default-presets.sh <mode>"
log "install complete"
