#!/usr/bin/env bash
# Optional full offline suite (CI runners / rare local). Default local gate:
#   task verify   # fmt + git diff --check only
# PR quality: .github/workflows/pr-fast.yml. Deep suite: .github/workflows/nightly.yml.
set -euo pipefail

export CI=1
export IMPETUS_NONINTERACTIVE=1
export RUST_BACKTRACE=1

# Never pass provider credentials into the verification environment.
unset OPENAI_API_KEY ANTHROPIC_API_KEY TAVILY_API_KEY EXA_API_KEY || true
unset GITHUB_TOKEN GH_TOKEN || true

printf '%s\n' '== fmt =='
cargo fmt --all -- --check

printf '%s\n' '== check =='
cargo check --workspace --all-targets

printf '%s\n' '== clippy =='
cargo clippy --workspace --all-targets -- -D warnings

printf '%s\n' '== deterministic tests (ignored live/auth smokes are not run by default) =='
cargo test --workspace --all-targets

printf '%s\n' '== guard: production Rust sources must not invoke common privilege escalators =='
if grep -RInE --include='*.rs' 'Command::new\("(sudo|doas|su)"\)|\.arg\("sudo"\)' crates; then
  echo 'privilege-escalation invocation found in Rust sources' >&2
  exit 1
fi

printf '%s\n' 'offline hardening verification passed'
