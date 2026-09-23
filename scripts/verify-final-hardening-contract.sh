#!/usr/bin/env bash
set -euo pipefail

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

pass() {
  echo "PASS: $*"
}

[[ -f .github/workflows/pr-fast.yml ]] || fail "missing .github/workflows/pr-fast.yml"
[[ -f .github/workflows/nightly.yml ]] || fail "missing .github/workflows/nightly.yml"

grep -Eq 'pull_request:' .github/workflows/pr-fast.yml || fail "PR Fast does not trigger on pull_request"
grep -Eq 'workflow_dispatch:' .github/workflows/pr-fast.yml || fail "PR Fast lacks workflow_dispatch"
grep -Eq 'cancel-in-progress:[[:space:]]*true' .github/workflows/pr-fast.yml || fail "PR Fast lacks cancel-in-progress"

if grep -Eq 'runs-on:[[:space:]]*macos' .github/workflows/pr-fast.yml; then
  fail "PR Fast still contains an automatic macOS runner"
fi

if grep -Eq 'cargo[[:space:]]+(test|clippy|audit|deny)' .github/workflows/pr-fast.yml; then
  fail "PR Fast still contains heavy test/clippy/security commands"
fi

pass "PR Fast shape"

grep -Eq 'schedule:' .github/workflows/nightly.yml || fail "Nightly lacks schedule"
grep -Eq '0[[:space:]]+23[[:space:]]+\*[[:space:]]+\*[[:space:]]+\*' .github/workflows/nightly.yml || fail "Nightly is not scheduled at 23:00 UTC (02:00 UTC+3)"
grep -Eq 'workflow_dispatch:' .github/workflows/nightly.yml || fail "Nightly lacks workflow_dispatch"
grep -Eq 'cargo[[:space:]]+test' .github/workflows/nightly.yml || fail "Nightly does not contain cargo test"
grep -Eq 'cargo[[:space:]]+clippy' .github/workflows/nightly.yml || fail "Nightly does not contain cargo clippy"
grep -Eq 'runs-on:[[:space:]]*macos' .github/workflows/nightly.yml || fail "Nightly lacks macOS validation"

pass "Nightly shape"

if [[ -f .github/workflows/ci.yml ]] && grep -Eq 'pull_request:' .github/workflows/ci.yml; then
  if grep -Eq 'runs-on:[[:space:]]*macos|cargo[[:space:]]+(test|clippy|audit|deny)' .github/workflows/ci.yml; then
    fail "legacy ci.yml still runs heavy PR work in parallel"
  fi
fi

if grep -R -n -E 'no approval flow yet|respond_permission.*not fully implemented' crates/impetus-acp-gateway/src 2>/dev/null; then
  fail "stale incomplete ACP permission path remains"
fi

if grep -R -n -E 'feature/issue-[0-9]+-extension-sdk' README.md TODO.md ARCHITECTURE.md EXTENSION_REPOSITORY_CONTRACT.md 2>/dev/null; then
  fail "docs still reference an already-merged extension feature branch"
fi

pass "Static hardening contract checks"

echo "Run project-specific nightly/full suites in GitHub Actions."
echo "Local default remains: cargo fmt --all -- --check && git diff --check"
