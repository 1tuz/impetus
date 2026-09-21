# Configuration

Daemon `impetusd` accepts optional flags:

```text
impetusd [--policy-config PATH] [--provider-profile PATH | --acp-profile PATH]
```

Without a provider/ACP flag, Impetus uses its mock streaming provider. Exactly
one backend flag may be set. `--policy-config` must appear before the backend
flag when both are used. The daemon rejects unknown arguments and profiles with
unknown fields.

## PolicyConfig

User JSON overrides on top of fail-closed defaults
([`policy_config.rs`](../../crates/impetus-core/src/policy_config.rs)).

Resolution order at daemon start:

1. `--policy-config PATH` (required file; bad JSON refuses start)
2. `IMPETUS_POLICY_CONFIG` (same — explicit, refuse on error)
3. `$IMPETUS_DATA_DIR/policy.json` (optional; missing = empty overrides)

Example:

```json
{
  "version": 1,
  "overrides": {
    "write_file": "allow",
    "spawn_process": "deny",
    "network_connect": "needs_approval"
  }
}
```

In-process reload remains a library API (`PolicyEngine::reload_config*` /
`AgentRuntime::reload_policy_config*`). Typed IPC reload is still open.

## Data and socket paths

| Setting | Default | Purpose |
| --- | --- | --- |
| `IMPETUS_DATA_DIR` | `~/Library/Application Support/Impetus` | SQLite event store directory; optional `policy.json`. |
| `IMPETUS_SOCKET` | `<data-dir>/harness.sock` | Unix-socket path used by daemon, `impetus` CLI, and Zap adapter. |
| `IMPETUS_POLICY_CONFIG` | (unset) | Explicit PolicyConfig JSON path (see above). |
| `IMPETUS_NONINTERACTIVE` | (unset) | When truthy (`1`/`true`/`yes`/`on`), Keychain resolver fails closed (no GUI). `CI=true` implies the same. |
| `IMPETUS_CREDENTIAL_BACKEND` | `keychain` | With `--provider-profile`: `mock` = `NoCredentialResolver`; `keychain` = macOS Keychain (only when interactive). |

## Explore + MCP autoload (daemon)

At startup `impetusd` always wires production Explore (`explore_spawn`) using the
daemon default provider (mock, `--provider-profile`, or `--acp-profile`). Child
runs use a restricted AgentLoop (`list`/`read`/`search` only); durable results
live in `$IMPETUS_DATA_DIR/child_results.sqlite3` and parent resume goes through
`Harness::complete_explore_and_gate`.

Optional MCP servers autoload from `$IMPETUS_DATA_DIR/mcp/*.json` (each file is
an [`McpModule`](../../crates/impetus-core/src/extension_compat.rs) JSON config).
Missing `mcp/` directory is fine (no MCP). Any present file must parse and
validate (`impetus.mcp.v1`); bad config refuses daemon start. Servers connect
lazily on first tool use (no marketplace).

The daemon creates the Unix socket with mode `0600`. It refuses to replace an
existing socket path, so stop the old daemon before starting another one at the
same path.

## Direct-provider profile

`config/provider-profile.example.json` shows the schema:

```json
{
  "id": "local.mock",
  "endpoint": "http://127.0.0.1:11434",
  "model": "mock-model",
  "credential_strategy": { "kind": "none" }
}
```

Supported fields are `id`, `endpoint`, `model`, `credential_strategy`, and
optional `openai_http_api` (`chat_completions` default, or `responses` for the
`/v1/responses` opt-in path). The endpoint must be an absolute URL without a
query string or fragment. By default the provider requests
`<endpoint>/v1/chat/completions` with streaming enabled. With
`"openai_http_api": "responses"` it requests `<endpoint>/v1/responses` instead.

### Credential strategies

| `kind` | Accepted endpoint | Required fields | Notes |
| --- | --- | --- | --- |
| `none` | Loopback `http` or `https` | none | For a local provider only. |
| `keychain_reference` | Non-empty HTTPS URL | `service`, `account` | The daemon reads the credential from the macOS Keychain only when making a request. |
| `system_browser_o_auth` | HTTPS URL | `authorization_url`, `token_url`, `client_id`, `keychain_service`, `keychain_account` | Profile validation exists; complete user-facing OAuth flow is not a documented setup path yet. |

Never add a token, private key, or passphrase to a profile, event, log, or test
fixture. The profile is an opaque locator, not a secret store.

## ACP profiles

[`docs/examples/acp-profile.example.json`](../examples/acp-profile.example.json)
describes an external ACP agent executable for:

```text
impetusd --acp-profile PATH
```

Rules:

- `command` must be an absolute path (for Codex: `codex-acp`, not plain `codex`).
- `credential_strategy` must be `{ "kind": "agent_owned" }`; no Keychain / raw
  tokens / secret env names on the Impetus profile.
- When the agent advertises auth methods, set `auth_method_id` explicitly
  (Codex ACP: `"api-key"` for API-key / custom-provider login, or `"chat-gpt"`
  for ChatGPT login). Impetus never picks a method implicitly.
- Codex credentials live in the agent's own home (`~/.codex`). Impetus inherits
  the process environment and does not inject API keys. If Codex `auth_mode` is
  `apikey`, `~/.codex/config.toml` must point `model_provider` at a provider
  whose `base_url` matches that key (otherwise Codex hits `api.openai.com` and
  fails with 401).

`config/agent-backends.example.json` (if present) is a planning catalog, not a
runtime file consumed by the daemon.

## TUI appearance

| Setting | Default | Purpose |
| --- | --- | --- |
| `IMPETUS_TUI_THEME` | `impetus` | Named theme id (or label). Catalog: Impetus Neon, Impetus Stars, Dracula, Nord, Gruvbox, Tokyo Night, Catppuccin Mocha, Solarized Dark, Monokai, One Dark, Matrix, Zinc. |
| `IMPETUS_TUI_NO_MOUSE` | unset | Disable mouse capture when set to `1`/`true`. |

In the TUI: `/theme` or F5 opens the picker; Ctrl+Shift+T cycles. Live keymap:
`?` / F1.
