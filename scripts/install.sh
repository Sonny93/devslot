#!/usr/bin/env bash
# Install or update ds and the devslot plugin from this clone. Safe to run again.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
readonly ROOT

install_binary() {
  (cd "$ROOT" && env -u RUSTUP_TOOLCHAIN cargo install --path "$ROOT" --locked)
}

resolve_git_path() {
  (cd "$ROOT" && cd "$(git rev-parse "$1")" && pwd -P)
}

is_linked_worktree() {
  [[ "$(resolve_git_path --git-dir)" != "$(resolve_git_path --git-common-dir)" ]]
}

is_main_branch() {
  [[ "$(git -C "$ROOT" symbolic-ref --short -q HEAD || true)" == "main" ]]
}

read_marketplace_path() {
  command -v python3 > /dev/null || return 0
  claude plugin marketplace list --json 2> /dev/null | python3 -I -c '
import json, sys
try:
    entries = json.load(sys.stdin)
except ValueError:
    sys.exit(0)
for entry in entries:
    if isinstance(entry, dict) and entry.get("name") == "devslot" and entry.get("path"):
        print(entry["path"])
        break
' || true
}

warn_other_clone() {
  local configured_path
  configured_path="$(read_marketplace_path)"
  [[ -z "$configured_path" || "$configured_path" == "$ROOT" ]] && return 0
  echo "warning: the devslot marketplace points at $configured_path, not $ROOT: the plugin comes from that clone." >&2
}

warn_checkout() {
  if is_linked_worktree || ! is_main_branch; then
    echo "warning: ds was built from $ROOT (linked worktree or branch other than main), while the plugin comes from the marketplace clone." >&2
  fi
}

install_plugin() {
  if claude plugin marketplace update devslot; then
    claude plugin update devslot@devslot
    return
  fi
  if is_linked_worktree; then
    echo "error: ds is installed, but the plugin step was refused: $ROOT is a linked worktree and would be deleted with it. Run scripts/install.sh from the main clone." >&2
    exit 1
  fi
  claude plugin marketplace add "$ROOT"
  claude plugin install devslot@devslot
}

install_binary

if ! command -v claude > /dev/null; then
  echo "ds is installed. claude is not on PATH, so the plugin step was skipped. Run:"
  echo "  claude plugin marketplace add $ROOT && claude plugin install devslot@devslot"
  echo "  (later: claude plugin marketplace update devslot && claude plugin update devslot@devslot)"
  exit 0
fi

warn_checkout
install_plugin
warn_other_clone
echo "Done: restart your Claude Code sessions to load the updated plugin."
