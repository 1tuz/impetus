# Contributing to Impetus

Thanks for improving Impetus. Keep changes narrow, reviewable, and aligned
with the harness-first boundary.

## Before you start

1. Read [README.md](README.md) and the relevant guide in `docs/`.
2. Check the current [roadmap](docs/ROADMAP.md) so planned work is not presented
   as a completed capability.
3. Discuss a substantial design or public-protocol change in an issue before
   investing in an implementation.

## Local setup and checks

Use macOS with Xcode, the Metal Toolchain, Rust `1.98.0`, and Task. Then run:

```zsh
task setup
task verify
```

`task verify` is required for Rust changes. If a change modifies
`Cargo.toml` or `Cargo.lock`, run `task security` as well. When changing CI,
toolchain, dependency policy, or the verification contract, update
`.gitlab-ci.yml` and exercise the relevant local CI job when available.

## Scope and safety

- Keep `impetus-core` free of native-GUI, terminal-rendering, and client-specific
  dependencies.
- Do not add raw credentials to the repository, profiles, SQLite fixtures,
  logs, or tests. Use opaque Keychain references.
- Preserve the decision boundary: `Policy → Deny | Allow | NeedsApproval`, then
  sandbox, capability, and execution.
- Do not describe a stub, planned interface, or reference UI as a production
  feature.

## Pull requests

Explain the user-visible change, its boundary, and the checks you ran. Keep
unrelated formatting, refactors, generated state, and secret-bearing files out
of the diff. Use the repository's commit-subject convention; `task
commit:check MESSAGE='ATM-123 docs: Описана схема'` validates it.

## Security issues

Do not open a public issue with a vulnerability or secret. Follow
[SECURITY.md](SECURITY.md).
