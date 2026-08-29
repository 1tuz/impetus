# OpenClaude overlay (this repo)

**Canonical repo rules:** `AGENTS.md` in repo root — OpenClaude loads it automatically (`PRIMARY_PROJECT_INSTRUCTION_FILE`). Follow it for harness boundaries, verify, CI, git, subagents.

Do not duplicate `AGENTS.md` here. Read it when scope, commits, or architecture are unclear.

## OpenClaude-only (not in AGENTS.md)

- Global agent rules: `~/.openclaude/CLAUDE.md` (RTK, token hygiene, language).
- Style/constitution detail: `~/.codewhale/constitution.json`, RTK: `~/.codewhale/RTK.md`.
- Tool failures / context bloat / loops: `/self-heal` skill.
- Verify shortcut in this repo: `rtk task verify` (same four Rust checks as in AGENTS.md).

## Autopilot (stop early-turn spam)

- **Do not end a turn with text only** if work remains — call `Read`/`Edit`/`Write`/`Bash` next.
- Never claim a file exists until `Write`/`Edit` succeeded.
- Long TODO runs: `/goal <one line condition>` then `/goal resume` if paused; or user says `продолжай`.

## Local repo tools (CloseRouter / openai shim)

- **Never `WebSearch`** for files in this repo (`TODO.md`, `docs/`, `crates/`). Use `Read`, `Grep`, `Glob` only.
- **No subagents by default** for TODO/roadmap work — main thread unless user explicitly asks for parallel agents.
- **`/goal`**: if paused with malformed JSON, run `/goal resume` after config reload; do not rely on web search mid-goal.
