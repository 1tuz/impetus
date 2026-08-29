# TUI Reference Audit (planned)

> **Status: audit not started.** This document is a plan and draft hypothesis
> table only. Decisions are **not locked** until a real source audit of
> [1jehuang/jcode](https://github.com/1jehuang/jcode) is completed.

Impetus не fork JCode и не импортирует его application/runtime layer.

**Принцип (target):**

```text
JCode   → reference implementation / UX patterns (after audit)
Impetus → собственный thin TUI client (Ratatui + Crossterm baseline)
```

**Codex** — secondary reference для composer, large paste, doctor/diagnostics,
approval UX, errors/remediation.

**Baseline stack (planned evaluation):** Ratatui, Crossterm.

---

## TODO — source audit (blocking)

Before treating any row below as final:

- [ ] Clone/check out актуальный `https://github.com/1jehuang/jcode`
- [ ] Зафиксировать audited **commit SHA** в этом файле
- [ ] Перечислить конкретные **файлы/модули** presentation layer (paths in repo)
- [ ] Для каждого component — решение `ADAPT | REIMPLEMENT | SKIP` с reason по коду, не по памяти
- [ ] Отметить gaps: что в JCode отсутствует и берётся только из Codex/terminal spec

**Audited commit:** _TBD_

**Audited paths:** _TBD_

---

## Decision legend

| Decision | Meaning |
| --- | --- |
| `ADAPT` | Перенести presentation-идею/pattern, собственная реализация на Ratatui |
| `REIMPLEMENT` | Тот же UX contract, чистая Impetus-реализация без копирования кода |
| `SKIP` | Не в scope standalone TUI или покрыто другим client (Zap) |

---

## Component audit (draft — pre-audit hypotheses)

_Replace with audited decisions after TODO above is complete._

| Component | Reference | Decision | Reason |
| --- | --- | --- | --- |
| Composer (single-line) | JCode?, Codex | TBD | Audit pending |
| Composer (multiline) | JCode?, Codex | TBD | Audit pending |
| Bracketed paste | Terminal spec | TBD | Required for multiline paste |
| Large paste detection | Codex | TBD | Threshold → compact label |
| Large paste upload | Codex flow, Impetus arch | TBD | Chunked → `ArtifactStore` → `ArtifactRef` |
| Keyboard handling | JCode? | TBD | Audit pending |
| Streaming output | JCode? | TBD | Audit pending |
| Markdown render | JCode? | TBD | Audit pending |
| Diff view | JCode? | TBD | Audit pending |
| Approval UI | JCode?, Codex | TBD | Typed harness approvals |
| Session picker | JCode? | TBD | `HarnessClient` |
| Fuzzy search | JCode? | TBD | Audit pending |
| Command palette | JCode?, Codex | TBD | Audit pending |
| Scrolling / scrollback | JCode? | TBD | Client buffer vs durable events |
| Terminal resize | Crossterm | TBD | Standard Ratatui layout |
| Status / usage UI | JCode?, Codex | TBD | Typed status APIs |
| Redraw / event coalescing | JCode? | TBD | Performance |
| Error + remediation | Codex | TBD | Align with `impetus doctor` |
| Agent Runtime | — | SKIP | `impetusd` only |
| Provider / session / tool authority | — | SKIP | Harness owns authority |
| PTY / terminal emulator | Zap | SKIP | Not a terminal emulator |

---

## Explicit non-goals (TUI)

- Fork or vendor JCode application code.
- Direct `impetus-core` / SQLite imports from TUI process.
- Custom ANSI terminal emulator, tabs, or scrollback as product core.
- Loading external agent CLIs from TUI bypassing harness policy.

---

## After audit

1. Update table with SHA, file paths, and locked decisions.
2. Ratatui spike: composer + stream + one approval mock.
3. Bracketed paste test matrix (iTerm, Terminal.app, SSH).
4. Large paste flow with durable `ArtifactStore` (depends on harness API).
