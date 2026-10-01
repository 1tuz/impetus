# Depending on `impetus-extension-sdk`

Primary recipe: crates.io `0.1.0`. Package is **dry-run ready** for publish
(`cargo publish -p impetus-extension-sdk --dry-run`); live upload waits on
`CARGO_REGISTRY_TOKEN` / `cargo login` (issue #449). Until the crate page
resolves, use the git pin fallback below.

## crates.io (recommended)

```toml
[dependencies]
impetus-extension-sdk = "0.1.0"
```

Pin a semver range if you need upgrade room (`"0.1"` / `"^0.1.0"`).

Do **not** depend on `impetus-core` / `impetusd` from an extension package.

## Git pin (fallback)

Use when crates.io is unreachable, you need an unreleased SHA, or publish is
still pending:

```toml
[dependencies]
impetus-extension-sdk = { git = "https://github.com/1tuz/impetus", rev = "<IMPETUS_GIT_SHA>", package = "impetus-extension-sdk" }
```

Rules:

- Prefer `rev = "<full sha>"` (immutable). Avoid floating `branch = "main"`.
- After Impetus cuts a release tag, `tag = "vX.Y.Z"` is also fine if the tag
  includes this crate.

Record the pinned SHA in the external repo README / lockfile.

## Path pin (local co-development)

```toml
[dependencies]
impetus-extension-sdk = { path = "../impetus/crates/impetus-extension-sdk" }
```

## What you compile against

- Manifest parse/validate (`ExtensionPackageManifest`)
- Permission / capability / entrypoint enums
- `host_protocol` constants for `host_process` children

You do **not** need the SDK at runtime for declarative `instruction_pack`
packages — only `extension.toml` + skills on disk. Use the SDK for author
unit tests and for `host_process` protocol constants.

## Smoke

From the Impetus checkout:

```zsh
cargo run -p impetus-extension-sdk --example validate_demo_pack
```

## IPC (daemon)

Package manage is daemon IPC (`extension_manage`, IPC ≥ 14) — see [ipc.md](./ipc.md).
Wire types: `impetus-protocol` (same crates.io / git pin if a client needs them).
