#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/internal-pr-format-gate.sh
  scripts/internal-pr-format-gate.sh --fix

Purpose:
  Cheap local guard for internal Rust PR flows. Refuses to continue when
  formatting is dirty, so format-only CI failures are caught before PR
  create/update churn.

Behavior:
  - default: runs `cargo fmt --all -- --check`
  - --fix: runs `cargo fmt --all`, then re-checks
USAGE
}

fix=0
case "${1:-}" in
  "") ;;
  --fix) fix=1 ;;
  -h|--help) usage; exit 0 ;;
  *)
    echo "unknown arg: $1" >&2
    usage >&2
    exit 1
    ;;
esac

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found on PATH" >&2
  exit 1
fi

if [ ! -f Cargo.toml ]; then
  echo "Cargo.toml not found in $(pwd); run this from a Rust repo root/worktree" >&2
  exit 1
fi

if [ "$fix" -eq 1 ]; then
  echo "[op-pi] auto-fixing format with cargo fmt --all"
  cargo fmt --all
fi

if cargo fmt --all -- --check; then
  echo "[op-pi] format gate passed"
  exit 0
fi

cat >&2 <<'MSG'
[op-pi] format gate failed
- Run: cargo fmt --all
- Re-run: scripts/internal-pr-format-gate.sh
- Or auto-fix: scripts/internal-pr-format-gate.sh --fix
Refusing to continue internal PR flow with a known format-only red CI path.
MSG
exit 1
