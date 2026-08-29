# Design references

A reference informs an approach; it never becomes a dependency, permission, or
claim of implementation automatically.

## Architectural / Agent Harness References

| Reference | What informs Impetus |
| --- | --- |
| [Codex](https://openai.com/codex/) | Tool orchestration, capability/policy boundaries, sandbox execution, structured tool lifecycle. |
| [Claude Code](https://code.claude.com/docs/) | Long-session compaction/recovery, autonomy/risk concepts, fewer unnecessary approvals. |
| [OpenClaude](https://github.com/Gitlawb/openclaude) | Per-agent budgets, reasoning effort, separate compaction model, context/token UX, useful multi-model concepts. Reuse always requires separate license/provenance review. |
| [jcode](https://github.com/1jehuang/jcode) | Persistent daemon, lightweight sessions, multi-model, swarm, memory, soft interrupt, low overhead, and Rust TUI ideas (UX reference for planned TUI audit). |
| [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) | Modular capability architecture and stable subsystem interfaces. |
| [Qwen Code](https://github.com/QwenLM/qwen-code) | Fork-context subagents, inherited prefix/cache, background delegation. |
| [Pi](https://github.com/badlogic/pi-mono) | Session tree/DAG, shared history, non-destructive context projection. |
| [OpenCode](https://github.com/anomalyco/opencode) | Checkpoints, diff/revert/fork, workspace transactions, repo-intelligence concepts. |
| [Aider](https://github.com/Aider-AI/aider) | Repo Map, ranked repository context, token-budgeted symbol/import graph, optional Architect → Editor. |
| [Kimi Code](https://github.com/MoonshotAI/kimi-cli) | Transport-neutral clients, isolated persistent subagents, compact worker-to-parent results. |
| [RTK](https://github.com/rtk-ai/rtk) | Deterministic tool-output reduction; structured summary for the model while raw output remains an artifact. |

## Terminal / Frontend

| Reference | What informs Impetus |
| --- | --- |
| [Zap](https://github.com/zerx-lab/zap) | Terminal/frontend UX and a host UI for the Impetus backend. |

## Protocols

| Reference | What informs Impetus |
| --- | --- |
| [Agent Client Protocol](https://agentclientprotocol.com/get-started/agents) and [Rust SDK](https://github.com/agentclientprotocol/rust-sdk) | External coding-agent adapter, negotiation, sessions, updates, permission/auth interaction. |
| [ACP content](https://agentclientprotocol.com/protocol/v1/content) | Negotiated image/resource blocks and typed attachment references. |

## Implementation libraries

| Reference | Status |
| --- | --- |
| [russh](https://github.com/Eugeny/russh) | Candidate low-level SSH transport for the remote target. |
| [portable-pty](https://crates.io/crates/portable-pty) | Candidate low-level controlled PTY capability. |

## Diagram design

| Reference | What informs Impetus |
| --- | --- |
| [diagram-design](https://github.com/cathrynlavery/diagram-design) | Editorial layout, visual hierarchy, accessible SVG, and request-flow diagrams. |

## Source rule

Before API-dependent code, verify exact version/commit, inspect real uses, and
record compatibility assumptions. A planned upstream feature is not proof that
it is implemented here.
