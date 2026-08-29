# TUI Reference Audit

Source audit для standalone Impetus TUI. JCode — primary UX reference;
Impetus не fork JCode и не импортирует его application/runtime layer.

**Принцип:**

```text
JCode   → reference implementation / UX patterns
Impetus → собственный thin TUI client (Ratatui + Crossterm baseline)
```

**Codex** — secondary reference для composer, large paste, doctor/diagnostics,
approval UX, errors/remediation.

**Baseline stack (planned evaluation):** Ratatui, Crossterm.

---

## Decision legend

| Decision | Meaning |
| --- | --- |
| `ADAPT` | Перенести presentation-идею/pattern, собственная реализация на Ratatui |
| `REIMPLEMENT` | Тот же UX contract, чистая Impetus-реализация без копирования кода |
| `SKIP` | Не в scope standalone TUI или покрыто другим client (Zap) |

---

## Component audit

| Component | Reference | Decision | Reason |
| --- | --- | --- | --- |
| Composer (single-line) | JCode, Codex | REIMPLEMENT | Core UX; must use `HarnessClient`, bracketed paste |
| Composer (multiline) | JCode, Codex | REIMPLEMENT | Enter vs Shift+Enter; no accidental submit on paste |
| Bracketed paste | Terminal spec | ADAPT | Required for correct multiline paste semantics |
| Large paste detection | Codex | REIMPLEMENT | Threshold → compact label, not full inline text |
| Large paste upload | Codex (flow), Impetus arch | REIMPLEMENT | Chunked → `impetusd` ArtifactStore → `ArtifactRef`; not giant IPC JSON |
| Keyboard handling | JCode | ADAPT | Modal shortcuts, focus model; no JCode runtime deps |
| Streaming output | JCode | ADAPT | Incremental render from harness events |
| Markdown render | JCode | ADAPT | Bounded rendering; full doc as artifact if huge |
| Diff view | JCode | ADAPT | Presentation only; diff content from harness/artifacts |
| Approval UI | JCode, Codex | REIMPLEMENT | Typed approvals/diffs from IPC; human origin only |
| Session picker | JCode | ADAPT | List/search sessions via `HarnessClient` |
| Fuzzy search | JCode | ADAPT | Sessions, commands, palette entries |
| Command palette | JCode, Codex | ADAPT | Client-local commands + forwarded harness actions |
| Scrolling / scrollback | JCode | REIMPLEMENT | Client-side buffer; harness owns durable events |
| Terminal resize | Crossterm | ADAPT | Standard Ratatui layout reflow |
| Status / usage UI | JCode, Codex | REIMPLEMENT | Model, tokens, connection; from typed status APIs |
| Redraw / event coalescing | JCode | ADAPT | Performance; coalesce stream chunks before full redraw |
| Error + remediation display | Codex | ADAPT | Align with `impetus doctor` remediation style |
| Doctor integration | Codex | REIMPLEMENT | `impetus doctor` in CLI first; TUI may surface summary |
| Agent Runtime | JCode | SKIP | Lives in `impetusd` only |
| Provider implementation | JCode | SKIP | `ProviderRegistry` in daemon |
| Session authority | JCode | SKIP | `impetusd` owns durable sessions |
| Tool authority / execution | JCode | SKIP | Policy → sandbox → execution in harness |
| Auth / credential state | JCode | SKIP | Keychain + daemon; client shows references only |
| PTY / terminal emulator | Zap | SKIP | Impetus TUI is not a terminal emulator |
| Plugin runtime in-process | Cordis/DeepSeek | SKIP | Extension adapter in daemon, not TUI |

---

## Explicit non-goals (TUI)

- Fork or vendor JCode application code.
- Direct `impetus-core` / SQLite imports from TUI process.
- Custom ANSI terminal emulator, tabs, or scrollback as product core.
- Loading external agent CLIs from TUI bypassing harness policy.

---

## Next steps

1. Clone/read JCode presentation layer; note file paths and patterns.
2. Ratatui spike: composer + stream + one approval mock.
3. Bracketed paste test matrix (iTerm, Terminal.app, SSH).
4. Large paste flow with ArtifactStore integration (depends on harness API).
5. Update this table as decisions lock during implementation.
