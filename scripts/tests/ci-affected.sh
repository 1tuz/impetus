#!/usr/bin/env bash
# Unit checks for scripts/ci-affected.sh path classification + transitive deps.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SCRIPT="$ROOT/scripts/ci-affected.sh"
fail=0

scope() {
  CHANGED_FILES="$1" bash "$SCRIPT"
}

val() {
  local key="$1"
  local out="$2"
  printf '%s\n' "$out" | awk -F= -v k="$key" '$1 == k { print substr($0, index($0, "=") + 1); exit }'
}

expect() {
  local label="$1" key="$2" want="$3" out="$4"
  local got
  got="$(val "$key" "$out")"
  if [[ "$got" != "$want" ]]; then
    echo "FAIL [$label] $key: want='$want' got='$got'" >&2
    echo "--- output ---" >&2
    echo "$out" >&2
    fail=1
  else
    echo "ok [$label] $key=$got"
  fi
}

contains() {
  local label="$1" hay="$2" needle="$3"
  case " $hay " in
    *" $needle "*) echo "ok [$label] contains $needle" ;;
    *)
      echo "FAIL [$label] missing '$needle' in '$hay'" >&2
      fail=1
      ;;
  esac
}

# --- docs-only ---
out="$(scope $'docs/guides/development.md\nREADME.md')"
expect docs rust false "$out"
expect docs docs_only true "$out"
expect docs security false "$out"
expect docs workspace false "$out"
expect docs macos false "$out"
expect docs packages "" "$out"

# --- deny.toml → security only ---
out="$(scope 'deny.toml')"
expect deny rust false "$out"
expect deny security true "$out"
expect deny workspace false "$out"
expect deny packages "" "$out"
expect deny check_packages "" "$out"

# --- Taskfile / githooks → no Rust ---
out="$(scope $'Taskfile.yml\n.githooks/pre-commit')"
expect hooks rust false "$out"
expect hooks workspace false "$out"
expect hooks security false "$out"

# --- non-CI scripts → no Rust ---
out="$(scope 'scripts/install.sh')"
expect scripts rust false "$out"
expect scripts workspace false "$out"

# --- leaf crate (no macOS platform paths) ---
out="$(scope 'crates/impetus-zap-adapter/src/lib.rs')"
expect leaf rust true "$out"
expect leaf workspace false "$out"
expect leaf macos false "$out"
expect leaf packages "-p impetus-zap-adapter" "$out"
expect leaf check_packages "-p impetus-zap-adapter" "$out"

# --- protocol leaf expands dependants, still no macOS ---
out="$(scope 'crates/impetus-protocol/src/types.rs')"
expect protocol rust true "$out"
expect protocol macos false "$out"
expect protocol packages "-p impetus-protocol" "$out"
cp="$(val check_packages "$out")"
contains protocol "$cp" "-p impetus-core"
contains protocol "$cp" "-p impetus-client"

# --- macOS-platform path trips macos=true ---
out="$(scope 'crates/impetus-core/src/execution/sandbox.rs')"
expect seatbelt rust true "$out"
expect seatbelt macos true "$out"
expect seatbelt packages "-p impetus-core" "$out"

out="$(scope 'crates/impetusd/tests/daemon_userspace_no_sudo.rs')"
expect nosudo rust true "$out"
expect nosudo macos true "$out"
expect nosudo packages "-p impetusd" "$out"

# --- acp-gateway → transitive dependants (fixed-point) ---
out="$(scope 'crates/impetus-acp-gateway/src/lib.rs')"
expect acp rust true "$out"
expect acp workspace false "$out"
expect acp packages "-p impetus-acp-gateway" "$out"
cp="$(val check_packages "$out")"
contains acp "$cp" "-p impetus-acp-gateway"
contains acp "$cp" "-p impetus-core"
contains acp "$cp" "-p impetusd"
contains acp "$cp" "-p impetus-client"
contains acp "$cp" "-p impetus"
contains acp "$cp" "-p impetus-cli"
contains acp "$cp" "-p impetus-zap-adapter"
contains acp "$cp" "-p impetus-tui"

# --- core includes tui in check_packages ---
out="$(scope 'crates/impetus-core/src/lib.rs')"
cp="$(val check_packages "$out")"
contains core "$cp" "-p impetus-tui"
contains core "$cp" "-p impetus-client"

# --- Cargo.toml → workspace + security + macos ---
out="$(scope 'Cargo.toml')"
expect cargo rust true "$out"
expect cargo workspace true "$out"
expect cargo security true "$out"
expect cargo macos true "$out"
expect cargo packages "--workspace" "$out"
expect cargo check_packages "--workspace" "$out"

# --- ci-affected.sh self-test still triggers workspace ---
out="$(scope 'scripts/ci-affected.sh')"
expect self rust true "$out"
expect self workspace true "$out"

# --- workflow change → full Rust workspace ---
out="$(scope '.github/workflows/ci.yml')"
expect workflow rust true "$out"
expect workflow workspace true "$out"

# --- site-only ---
out="$(scope 'site/package.json')"
expect site rust false "$out"
expect site site true "$out"
expect site docs_only false "$out"

# --- reverse dependant: client change pulls tui into check_packages ---
out="$(scope 'crates/impetus-client/src/lib.rs')"
expect client rust true "$out"
cp="$(val check_packages "$out")"
contains client "$cp" "-p impetus-client"
contains client "$cp" "-p impetus-tui"

if [[ "$fail" -ne 0 ]]; then
  echo "ci-affected tests FAILED" >&2
  exit 1
fi
echo "ci-affected tests passed"
