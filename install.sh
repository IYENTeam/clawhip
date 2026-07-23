#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GITHUB_REPO="IYENTeam/op_pi"
INSTALLER_URL="${OP_PI_INSTALLER_URL:-https://github.com/${GITHUB_REPO}/releases/latest/download/op_pi-installer.sh}"
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export CARGO_HOME
SYSTEMD=0
SKIP_STAR_PROMPT=0

usage() {
  cat <<'EOF'
Usage: ./install.sh [--systemd] [--skip-star-prompt]

Options:
  --systemd            Install and start the bundled systemd service.
  --skip-star-prompt   Disable the optional post-install GitHub star prompt.
  -h, --help           Show this help text.

Environment:
  OP_PI_INSTALLER_URL=<url>
      Override the prebuilt installer URL.
  OP_PI_SKIP_STAR_PROMPT=1
      Disable the optional post-install GitHub star prompt.
  OP_PI_WEBHOOK_URL=<url>
      Provide the Discord webhook URL for quick-start setup.

EOF
}

parse_args() {
  for arg in "$@"; do
    case "$arg" in
      --systemd) SYSTEMD=1 ;;
      --skip-star-prompt) SKIP_STAR_PROMPT=1 ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        echo "unknown arg: $arg" >&2
        usage >&2
        exit 1
        ;;
    esac
  done
}

log() {
  echo "[op_pi] $*"
}

is_truthy() {
  case "${1:-}" in
    1|true|TRUE|yes|YES|on|ON) return 0 ;;
    *) return 1 ;;
  esac
}

star_prompt_disabled() {
  [[ "$SKIP_STAR_PROMPT" == "1" ]] || is_truthy "${OP_PI_SKIP_STAR_PROMPT:-}"
}

is_interactive_install() {
  [[ -t 0 && -t 1 ]]
}

can_use_github_cli_for_star() {
  command -v gh >/dev/null 2>&1 && gh auth status &>/dev/null
}

star_repo_with_gh() {
  gh api --method PUT "/user/starred/${GITHUB_REPO}" --silent &>/dev/null
}

prompt_to_star_repo() {
  local response
  printf '[op_pi] Would you like to star %s on GitHub with gh? [y/N]: ' "$GITHUB_REPO"
  read -r response || return 0

  case "$response" in
    [yY]|[yY][eE][sS])
      if star_repo_with_gh; then
        log "thanks for starring ${GITHUB_REPO}"
      else
        log "unable to star ${GITHUB_REPO} with gh; continuing without it"
      fi
      ;;
    *)
      log "skipping GitHub star step"
      ;;
  esac
}

maybe_prompt_to_star_repo() {
  if star_prompt_disabled; then
    log "skipping GitHub star prompt (--skip-star-prompt or OP_PI_SKIP_STAR_PROMPT)"
    return 0
  fi

  if ! is_interactive_install; then
    return 0
  fi

  if ! can_use_github_cli_for_star; then
    return 0
  fi

  log "optional: star ${GITHUB_REPO} on GitHub to support the project"
  prompt_to_star_repo
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
[op_pi] A prebuilt binary was not available and Cargo is not installed.
[op_pi] Install Rust with rustup, then rerun this installer:
[op_pi]   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
[op_pi]   source "$HOME/.cargo/env"
MSG
    exit 1
  fi

  log "building from source with cargo install --path . --force"
  cd "$REPO_ROOT"
  cargo install --path . --force
}

sync_plugins() {
  local source_dir="$REPO_ROOT/plugins"
  local target_dir="$HOME/.op_pi/plugins"

  if [[ ! -d "$source_dir" ]]; then
    return 0
  fi

  mkdir -p "$target_dir"
  cp -R "$source_dir"/. "$target_dir"/
  log "synced plugins to $target_dir"
}

installed_binary_path() {
  if [[ -x "$CARGO_HOME/bin/op_pi" ]]; then
    printf '%s\n' "$CARGO_HOME/bin/op_pi"
    return 0
  fi

  if command -v op_pi >/dev/null 2>&1; then
    command -v op_pi
    return 0
  fi

  return 1
}

setup_quick_start() {
  local binary_path
  binary_path="$(installed_binary_path)" || return 0

  local config_path="$HOME/.op_pi/config.toml"
  if [[ -f "$config_path" ]]; then
    log "existing config found at $config_path; skipping quick-start scaffold"
    return 0
  fi

  local webhook_url="${OP_PI_WEBHOOK_URL:-}"
  if [[ -z "${webhook_url// }" && -t 0 ]]; then
    printf '[op_pi] Discord webhook URL (recommended quick start; press Enter to skip): '
    read -r webhook_url || true
  fi

  if [[ -n "${webhook_url// }" ]]; then
    log "scaffolding webhook quick-start config"
    "$binary_path" setup --webhook "$webhook_url"
    log "webhook config scaffolded at $config_path"
  else
    log "recommended quick start: op_pi setup --webhook 'https://discord.com/api/webhooks/...'"
    log "bot-token mode is still supported via ~/.op_pi/config.toml"
  fi
}

install_systemd_binary() {
  local binary_path
  binary_path="$(installed_binary_path)" || {
    log "unable to find installed op_pi binary for systemd setup"
    exit 1
  }

  log "installing $binary_path to /usr/local/bin/op_pi for systemd"
  sudo install -m 755 "$binary_path" /usr/local/bin/op_pi
}

main() {
  parse_args "$@"

  log "install flow: prebuilt binary -> cargo fallback -> SKILL attach -> config scaffold -> optional post-install GitHub star prompt -> verification"
  log "repo root: $REPO_ROOT"

  if install_prebuilt_binary; then
    log "prebuilt binary installed successfully"
  else
    install_from_source
  fi

  mkdir -p "$HOME/.op_pi"
  log "ensured config dir $HOME/.op_pi"
  sync_plugins
  log "next: read SKILL.md and attach the skill surface"
  setup_quick_start

  if [[ "$SYSTEMD" == "1" ]]; then
    install_systemd_binary
    sudo cp deploy/op_pi.service /etc/systemd/system/op_pi.service
    sudo systemctl daemon-reload
    sudo systemctl enable --now op_pi
    log "systemd unit installed and started"
  fi

  maybe_prompt_to_star_repo

  log "recommended verification: scripts/live_verify_default_presets.sh <mode>"
  log "install complete"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
