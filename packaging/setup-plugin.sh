#!/usr/bin/env bash
# Point Claude Code at the devslot plugin that matches the installed ds. Safe to run again.
set -euo pipefail

readonly MARKETPLACE_NAME="devslot"
readonly MARKETPLACE_REPO="Sonny93/devslot"
readonly PLUGIN_ID="devslot@devslot"
readonly UNPUBLISHED_VERSION="0.0.0"

fail() {
  echo "error: $*" >&2
  exit 1
}

read_ds_version() {
  command -v ds > /dev/null || fail "ds is not on PATH. Install it first, then run this script again."
  local output
  output="$(ds --version)"
  if [[ ! "$output" =~ ^ds\ ([0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?)$ ]]; then
    fail "unexpected output of 'ds --version': $output"
  fi
  if [[ "${BASH_REMATCH[1]}" == "$UNPUBLISHED_VERSION" ]]; then
    fail "this ds is $UNPUBLISHED_VERSION, a build that belongs to no release: there is no matching plugin to install."
  fi
  echo "${BASH_REMATCH[1]}"
}

print_manual_commands() {
  echo "  claude plugin marketplace add $1"
  echo "  claude plugin install $PLUGIN_ID"
}

classify_marketplace() {
  local marketplace_source="$1" listing
  command -v python3 > /dev/null || { echo unknown; return; }
  listing="$(claude plugin marketplace list --json 2> /dev/null)" || { echo unknown; return; }
  printf '%s' "$listing" | python3 -I -c '
import json, sys
name, repo, ref = sys.argv[1:4]
try:
    entries = json.load(sys.stdin)
except ValueError:
    print("unknown")
    sys.exit(0)
if not isinstance(entries, list):
    print("unknown")
    sys.exit(0)
for entry in entries:
    if isinstance(entry, dict) and entry.get("name") == name:
        same = entry.get("source") == "github" and entry.get("repo") == repo and entry.get("ref") == ref
        print("same" if same else "other")
        sys.exit(0)
print("none")
' "$MARKETPLACE_NAME" "$MARKETPLACE_REPO" "${marketplace_source#*@}" || echo unknown
}

is_plugin_installed() {
  command -v python3 > /dev/null || return 1
  claude plugin list --json 2> /dev/null | python3 -I -c '
import json, sys
try:
    entries = json.load(sys.stdin)
except ValueError:
    sys.exit(1)
sys.exit(0 if any(isinstance(entry, dict) and entry.get("id") == sys.argv[1] for entry in entries) else 1)
' "$PLUGIN_ID"
}

install_or_update_plugin() {
  if is_plugin_installed; then
    claude plugin update "$PLUGIN_ID"
    return
  fi
  claude plugin install "$PLUGIN_ID"
}

main() {
  local version marketplace_source state
  version="$(read_ds_version)"
  marketplace_source="$MARKETPLACE_REPO@v$version"

  if ! command -v claude > /dev/null; then
    echo "claude is not on PATH, so the plugin step was skipped. Once it is installed, run:"
    print_manual_commands "$marketplace_source"
    exit 0
  fi

  state="$(classify_marketplace "$marketplace_source")"
  case "$state" in
    same)
      claude plugin marketplace update "$MARKETPLACE_NAME"
      ;;
    other)
      echo "The $MARKETPLACE_NAME marketplace points at another source or version: replacing it with $marketplace_source."
      claude plugin marketplace remove "$MARKETPLACE_NAME"
      claude plugin marketplace add "$marketplace_source"
      ;;
    none)
      claude plugin marketplace add "$marketplace_source"
      ;;
    *)
      echo "error: could not read the configured marketplaces (python3 and a JSON listing are needed). Run:" >&2
      print_manual_commands "$marketplace_source" >&2
      exit 1
      ;;
  esac
  install_or_update_plugin
  echo "Done: restart your Claude Code sessions to load the plugin for ds $version."
}

main
