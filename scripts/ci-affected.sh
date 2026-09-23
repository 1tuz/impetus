#!/usr/bin/env bash
# Compute CI scope from changed files (vs BASE_REF, default origin/main).
# Prints key=value lines suitable for GitHub Actions $GITHUB_OUTPUT.
set -euo pipefail

BASE_REF="${BASE_REF:-origin/main}"
if [[ -n "${CHANGED_FILES:-}" ]]; then
  CHANGED="$CHANGED_FILES"
elif git rev-parse --verify "$BASE_REF" >/dev/null 2>&1; then
  CHANGED="$(git diff --name-only "${BASE_REF}...HEAD" 2>/dev/null || git diff --name-only "$BASE_REF" HEAD || true)"
else
  CHANGED="$(git diff --name-only HEAD~1 HEAD 2>/dev/null || true)"
fi

rust=false
docs=false
site=false
workspace=false
security=false
macos=false
pkgs=""

has_pkg() {
  case " $pkgs " in
    *" $1 "*) return 0 ;;
    *) return 1 ;;
  esac
}

mark_pkg() {
  has_pkg "$1" || pkgs="${pkgs:+$pkgs }$1"
}

mark_macos() {
  macos=true
}

while IFS= read -r f; do
  [[ -z "$f" ]] && continue
  case "$f" in
    site/*)
      site=true
      ;;
    docs/*|*.md|assets/*|LICENSE|SECURITY.md|CONTRIBUTING.md|AGENTS.md)
      docs=true
      ;;
    # License/policy only — no Rust compile.
    deny.toml)
      security=true
      ;;
    Cargo.toml|Cargo.lock|rust-toolchain|rust-toolchain.toml|.cargo/*)
      workspace=true
      rust=true
      macos=true
      case "$f" in
        Cargo.toml|Cargo.lock) security=true ;;
      esac
      ;;
    # Self-test path: selector + workflow changes need Rust (workspace for safety).
    .github/workflows/pr-fast.yml|.github/workflows/nightly.yml|.github/workflows/ci.yml|scripts/ci-affected.sh|scripts/tests/ci-affected.sh)
      rust=true
      workspace=true
      ;;
    # Tooling / hooks / non-CI scripts — no Rust compilation.
    Taskfile.yml|.githooks/*|scripts/*)
      ;;
    # --- macOS-platform sensitive paths ---
    crates/impetus-core/src/execution/sandbox.rs|\
    crates/impetus-core/src/execution/pty.rs|\
    crates/impetus-core/src/execution/process.rs|\
    crates/impetus-core/src/auth.rs|\
    crates/impetus-core/src/privilege_boundaries.rs|\
    crates/impetus-core/tests/macos_sandbox_*|\
    crates/impetus-core/tests/approval_seatbelt_e2e.rs|\
    crates/impetusd/src/peer_isolation.rs|\
    crates/impetusd/tests/daemon_userspace_no_sudo.rs|\
    crates/impetusd/tests/daemon_peer_isolation.rs|\
    crates/impetus-acp-gateway/src/profile.rs)
      rust=true
      mark_macos
      case "$f" in
        crates/impetus-core/*) mark_pkg impetus-core ;;
        crates/impetusd/*) mark_pkg impetusd ;;
        crates/impetus-acp-gateway/*) mark_pkg impetus-acp-gateway ;;
      esac
      ;;
    crates/impetus-protocol/*) rust=true; mark_pkg impetus-protocol ;;
    crates/impetus-extension-sdk/*) rust=true; mark_pkg impetus-extension-sdk ;;
    crates/impetus-core/*) rust=true; mark_pkg impetus-core ;;
    crates/impetus-acp-gateway/*) rust=true; mark_pkg impetus-acp-gateway ;;
    crates/impetus-client/*) rust=true; mark_pkg impetus-client ;;
    crates/impetus-tui/*) rust=true; mark_pkg impetus-tui ;;
    crates/impetus/*) rust=true; mark_pkg impetus ;;
    crates/impetusd/*) rust=true; mark_pkg impetusd ;;
    crates/impetus-cli/*) rust=true; mark_pkg impetus-cli ;;
    crates/impetus-zap-adapter/*) rust=true; mark_pkg impetus-zap-adapter ;;
    crates/test-module/*) rust=true; mark_pkg test-module ;;
    crates/*)
      rust=true
      workspace=true
      macos=true
      ;;
  esac
done <<< "$CHANGED"

# Direct reverse-deps (who depends on this package).
direct_dependants() {
  case "$1" in
    impetus-protocol)
      echo "impetus-core impetus-client impetusd impetus-extension-sdk"
      ;;
    impetus-extension-sdk)
      echo "impetus-core"
      ;;
    impetus-acp-gateway)
      echo "impetus-core impetusd"
      ;;
    impetus-core)
      echo "impetus-client impetus impetusd impetus-cli impetus-zap-adapter impetus-tui"
      ;;
    impetus-client)
      echo "impetus-tui impetus impetus-cli impetus-zap-adapter"
      ;;
    impetus-tui)
      echo "impetus"
      ;;
  esac
}

dependants=""
add_dep() {
  case " $dependants " in
    *" $1 "*) ;;
    *) dependants="${dependants:+$dependants }$1" ;;
  esac
}

in_scope() {
  case " $pkgs $dependants " in
    *" $1 "*) return 0 ;;
    *) return 1 ;;
  esac
}

# Fixed-point expansion: leaf → parents → their parents.
frontier="$pkgs"
while [[ -n "$frontier" ]]; do
  next=""
  for p in $frontier; do
    # shellcheck disable=SC2046
    for d in $(direct_dependants "$p"); do
      if in_scope "$d"; then
        continue
      fi
      add_dep "$d"
      next="${next:+$next }$d"
    done
  done
  frontier="$next"
done

if [[ "$workspace" == true ]]; then
  packages="--workspace"
  check_packages="--workspace"
  macos=true
elif [[ "$rust" == true ]]; then
  packages=""
  for p in $pkgs; do
    packages="${packages:+$packages }-p $p"
  done
  if [[ -z "$packages" ]]; then
    packages="--workspace"
    check_packages="--workspace"
    workspace=true
    macos=true
  else
    check_packages="$packages"
    for d in $dependants; do
      case " $check_packages " in
        *" -p $d "*) ;;
        *) check_packages="${check_packages:+$check_packages }-p $d" ;;
      esac
    done
  fi
else
  packages=""
  check_packages=""
fi

docs_only=false
if [[ "$rust" == false && "$site" == false ]]; then
  docs_only=true
fi

echo "rust=$rust"
echo "docs_only=$docs_only"
echo "site=$site"
echo "workspace=$workspace"
echo "security=$security"
echo "macos=$macos"
echo "packages=$packages"
echo "check_packages=$check_packages"
