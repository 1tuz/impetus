# Troubleshooting

## `connect harness socket` fails

Start the daemon before using `impetus-cli` or `impetus-zap-adapter`:

```zsh
cargo run -p impetus
```

The client and daemon must use the same `IMPETUS_SOCKET` value. If neither sets
it, both default to
`~/Library/Application Support/Impetus/harness.sock`.

## The daemon refuses to replace a socket

Another daemon may still be running, or a previous run left a socket at the
configured path. Stop the process that owns the socket before starting a new
daemon. Do not delete a socket until you have confirmed that no daemon is using
it.

## A provider profile is rejected

Check these constraints:

- `id` and `model` are non-empty.
- `endpoint` is an absolute URL without a query string or fragment.
- A `none` credential strategy points only to `localhost`, `127.0.0.1`, or
  `::1`.
- `keychain_reference` profiles use HTTPS and non-empty `service` and
  `account` fields.
- The file contains no extra fields and no raw credential.

Compare the file with `config/provider-profile.example.json` and read
[configuration](configuration.md).

## A Keychain-backed provider cannot authenticate

The daemon resolves the configured `service` and `account` only when it sends a
provider request. Confirm that the matching generic-password entry exists in
the macOS Keychain and that the process has permission to read it. The returned
error is deliberately redacted; do not paste credentials into an issue or log.

## A planned interface returns `Unavailable`

The IPC protocol advertises attachment and approval-detail requests, but their
backing storage/detail work is still on the roadmap. Do not depend on these
endpoints as a complete public API yet.

## CI behaves differently from `task verify`

GitLab CI runs a narrower unit-test scope for Linux Docker execution. Local
`task verify` runs `cargo test --workspace`, including the macOS integration
tests. See [development](development.md) for the exact command sets.
