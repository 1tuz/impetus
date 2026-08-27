#!/bin/zsh
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" || ! -x /usr/bin/sandbox-exec ]]; then
  print -u2 -- 'macOS Seatbelt smoke requires /usr/bin/sandbox-exec'
  exit 1
fi

spike_root="$(mktemp -d /tmp/agentic-terminal-seatbelt.XXXXXX)"
trap 'rm -rf "$spike_root"' EXIT
workspace="$spike_root/workspace"
mkdir "$workspace"
workspace="$(realpath "$workspace")"
allowed="$workspace/allowed.txt"
blocked="$spike_root/blocked.txt"
profile="(version 1)
(deny default)
(allow process-exec)
(allow file-read*)
(allow file-write* (subpath \"$workspace\"))"

/usr/bin/sandbox-exec -p "$profile" /usr/bin/touch "$allowed"
[[ -f "$allowed" ]]

if /usr/bin/sandbox-exec -p "$profile" /usr/bin/touch "$blocked"; then
  print -u2 -- 'Seatbelt unexpectedly allowed a write outside the workspace'
  exit 1
fi
[[ ! -e "$blocked" ]]
print -- 'macOS Seatbelt smoke passed: workspace write allowed; sibling write denied'
