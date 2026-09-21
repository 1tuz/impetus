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

while IFS= read -r f; do
  [[ -z "$f" ]] && continue
  case "$f" in
    site/*)
      site=true
      ;;
    docs/*|*.md|assets/*|LICENSE|SECURITY.md|CONTRIBUTING.md|AGENTS.md)
      docs=true
      ;;
    Cargo.toml|Cargo.lock|rust-toolchain|rust-toolchain.toml|.cargo/*|deny.toml)
      workspace=true
      rust=true
      case "$f" in
        Cargo.toml|Cargo.lock|deny.toml) security=true ;;
      esac
      ;;
    .github/workflows/ci.yml|.github/workflows/security.yml|scripts/ci-affected.sh)
      rust=true
      workspace=true
      ;;
    crates/impetus-core/*) rust=true; mark_pkg impetus-core ;;
    crates/impetus-acp-gateway/*) rust=true; mark_pkg impetus-acp-gateway ;;
    crates/impetus-client/*) rust=true; mark_pkg impetus-client ;;
    crates/impetus-tui/*) rust=true; mark_pkg impetus-tui ;;
    crates/impetus/*) rust=true; mark_pkg impetus ;;
    crates/impetusd/*) rust=true; mark_pkg impetusd ;;
    crates/impetus-cli/*) rust=true; mark_pkg impetus-cli ;;
    crates/impetus-zap-adapter/*) rust=true; mark_pkg impetus-zap-adapter ;;
    crates/test-module/*) rust=true; mark_pkg test-module ;;
    crates/*|.githooks/*|Taskfile.yml|scripts/*)
      rust=true
      workspace=true
      ;;
  esac
done <<< "$CHANGED"

dependants=""
add_dep() {
  case " $dependants " in
    *" $1 "*) ;;
    *) dependants="${dependants:+$dependants }$1" ;;
  esac
}

for p in $pkgs; do
  case "$p" in
    impetus-acp-gateway)
      add_dep impetus-core
      add_dep impetusd
      ;;
    impetus-core)
      add_dep impetus-client
      add_dep impetus
      add_dep impetusd
      add_dep impetus-cli
      add_dep impetus-zap-adapter
      ;;
    impetus-client)
      add_dep impetus-tui
      add_dep impetus
      add_dep impetus-cli
      add_dep impetus-zap-adapter
      ;;
    impetus-tui)
      add_dep impetus
      ;;
  esac
done

if [[ "$workspace" == true ]]; then
  packages="--workspace"
  check_packages="--workspace"
elif [[ "$rust" == true ]]; then
  packages=""
  for p in $pkgs; do
    packages="${packages:+$packages }-p $p"
  done
  if [[ -z "$packages" ]]; then
    packages="--workspace"
    check_packages="--workspace"
    workspace=true
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
echo "packages=$packages"
echo "check_packages=$check_packages"
