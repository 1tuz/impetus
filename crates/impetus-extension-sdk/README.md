# impetus-extension-sdk

Stable public SDK types for external
[`impetus-extensions`](https://github.com/1tuz/impetus) authors.

No UI, rusqlite, harness, or daemon dependencies. Host discovery, policy, and
AgentLoop wiring live in `impetus-core`.

## Add as a dependency

```toml
[dependencies]
impetus-extension-sdk = "0.1.0"
```

Git `rev` / path pin remains a fallback — see
[depending-on-sdk.md](https://github.com/1tuz/impetus/blob/main/docs/extensions/depending-on-sdk.md).

## What you get

- Manifest parse/validate (`ExtensionPackageManifest`)
- Permission / capability / entrypoint enums
- `host_protocol` constants for `host_process` children
- Compatibility helpers (`check_compatibility`)

Declarative `instruction_pack` packages need only `extension.toml` + skills on
disk at runtime. Use this crate for author unit tests and host-process protocol
constants.

## License

Apache-2.0. See [LICENSE](LICENSE).
