# Testing an extension

## Unit

- Parse/validate manifests with `ExtensionPackageManifest::from_toml_str`.
- Assert `check_compatibility` for supported and unsupported API versions.
- Assert permission uniqueness / unknown token rejection.

## Integration

- Drop package into a temp discovery root.
- Drive host load → enable → capability list → disable.
- Optional: run against local `impetusd` E2E (owned by core CI).

Reference fixture: `crates/impetus-extension-sdk/fixtures/demo-pack/`
(also under `crates/impetus-core/test-fixtures/extensions/` for host tests).
