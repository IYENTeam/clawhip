#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
hooks_dir="$repo_root/.git/hooks"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

install -m 0755 "$script_dir/post_commit.sh" "$hooks_dir/post-commit"
install -m 0755 "$script_dir/post_checkout.sh" "$hooks_dir/post-checkout"

echo "Installed op_pi example git hooks into $hooks_dir"
echo "Optional: export OP_PI_CHANNEL=<discord-channel-id> inside your shell or hook wrapper."
