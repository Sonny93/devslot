#!/usr/bin/env bash
# Install ds from this release tarball, then set up the matching Claude Code plugin.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
readonly HERE
readonly BIN_DIR="${DEVSLOT_BIN_DIR:-$HOME/.local/bin}"

mkdir -p "$BIN_DIR"
install -m 0755 "$HERE/ds" "$BIN_DIR/ds"
echo "ds installed in $BIN_DIR"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "warning: $BIN_DIR is not on your PATH, and the plugin hooks call ds by name: add it to your shell profile." >&2 ;;
esac

PATH="$BIN_DIR:$PATH" "$HERE/setup-plugin.sh"
