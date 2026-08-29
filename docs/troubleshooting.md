# Troubleshooting

## `connect harness socket` / `Failed to connect to impetusd` fails

Start the daemon before the CLI client or Zap adapter:

```zsh
cargo run -p impetusd
# or, after install:
impetusd
```

Client and daemon must share `IMPETUS_SOCKET`. Default:
`~/Library/Application Support/Impetus/harness.sock`.

## The daemon refuses to replace a socket

Another `impetusd` may still be running, or a previous run left a socket at the
configured path. Stop the owning process before starting a new daemon. Do not
delete a socket until you have confirmed no daemon is using it.

## A provider profile is rejected

Check these constraints:

- `id` and `model` are non-empty.
- `endpoint` is an absolute URL without a query string or fragment.
- A `none` credential strategy points only to `localhost`, `127.0.0.1`, or
  `::1`.
- `keychain_reference` profiles use HTTPS and non-empty `service` and
  `account` fields.
- The file contains no extra fields and no raw credential.

Compare with `config/provider-profile.example.json` and [configuration](configuration.md).

## A Keychain-backed provider cannot authenticate

`impetusd` resolves `service` and `account` only when sending a provider
request. Confirm the generic-password entry exists and the process may read it.
Errors are redacted; do not paste credentials into issues or logs.

## A planned interface returns `Unavailable`

The IPC protocol advertises attachment and approval-detail requests, but backing
work is still on the roadmap. Do not treat these as a complete public API yet.

## Diagnostics (planned)

`impetus doctor` and `impetus doctor --json` will report versions, socket, IPC
compatibility, store health, providers, modules, and remediation hints. Not
implemented yet — see [TODO.md](../TODO.md) Phase 1.

## CI behaves differently from `task verify`

GitLab CI runs a narrower unit-test scope for Linux Docker execution. Local
`task verify` runs `cargo test --workspace`, including macOS integration tests.
See [development](development.md).
