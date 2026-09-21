# Design references

A reference informs an approach. It is not a dependency, permission grant, or
proof that Impetus implements the same feature.

Prefer, in order: official specs/repos, public protocols, well-maintained
open-source harnesses. Document **why a design is correct for Impetus**, not a
list of projects copied from.

## Protocols and libraries

| Reference | Role for Impetus |
| --- | --- |
| [Agent Client Protocol](https://agentclientprotocol.com/get-started/agents) / [Rust SDK](https://github.com/agentclientprotocol/rust-sdk) | External coding-agent adapter (ACP). |
| [OpenAI Chat Completions API](https://platform.openai.com/docs/api-reference/chat) | Streaming + tool-call shapes (Chat Completions path). |
| [Anthropic Messages API](https://docs.anthropic.com/en/api/messages) | Streaming + `tool_use` shapes. |
| [Ratatui](https://crates.io/crates/ratatui) `0.30.2` + [Crossterm](https://crates.io/crates/crossterm) `0.29.0` | Adopted for `impetus-tui` ([ratatui-spike.md](../archive/ratatui-spike.md)). |
| [russh](https://github.com/Eugeny/russh) | Candidate SSH transport (remote tier). |
| [portable-pty](https://crates.io/crates/portable-pty) | Candidate controlled PTY capability. |

## Harness ideas (optional study)

Use only when a concrete Impetus need exists. Do not treat as a parity checklist.

| Theme | Useful principle |
| --- | --- |
| Tool orchestration | Central policy gate; structured tool lifecycle; sandbox retry clarity. |
| Long sessions | Compaction as durable state transition; never put permissions only in summaries. |
| Multi-role agents | Explicit roles + metadata (tools, write roots, budgets) outside the prompt. |
| Context | Event-projected context; token-budgeted selection; artifacts by reference. |
| Output reduction | Bounded previews to the model; full raw body as durable artifact. |
| Terminal hosts | Thin client; host UI does not own SQLite/policy/secrets. |

Official product docs and public harness source may be inspected privately while
designing. They should not appear as “Impetus was inspired by X” unless there is
a real protocol, license, or compatibility reason.

## Source rule

Before API-dependent code: pin version/commit, inspect real behaviour, record
compatibility assumptions. A planned upstream feature is not proof it exists here.
