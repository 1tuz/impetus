# TODO — Impetus Harness

Short-term work queue. See `docs/ROADMAP.md` and `ARCHITECTURE.md` for context.

## Foundation (done)
- [x] Headless runtime with SQLite WAL and durable Event Log
- [x] Versioned local IPC via Unix Domain Socket
- [x] `HarnessClient` for Unix transport and in-memory tests
- [x] Durable sessions and client reconnect
- [x] Cursor backfill + push event subscription
- [x] Policy engine with `Deny | Allow | NeedsApproval`
- [x] Typed `Action` with `origin=user|agent` and request ID tracking
- [x] Sandbox integration for shell/process capabilities
- [x] Attachment/diff/detail DTO backing services with bounded storage
- [x] First autonomous Agent Loop and Tool Orchestrator slice

## In Progress

### 3. Durable budgets and rules-based Model Router
- [ ] Token and wall-time budget tracking per session
- [ ] Budget enforcement in agent loop
- [ ] Model Router foundation with provider selection rules
- [ ] Cost estimation and budget warnings

### 4. Standalone `impetus` CLI/TUI and Zap integration
- [x] Binary topology: `impetus` (client) and `impetusd` (daemon)
- [x] `impetus` CLI using `HarnessClient` → `impetusd`
- [x] Release pipeline builds both binaries
- [x] Install script deploys `impetus` + `impetusd`
- [ ] `impetus` auto-discovers and safely spawns `impetusd` if needed
- [ ] Basic TUI for interactive sessions
- [ ] Zap discovery/connect/authorize protocol
- [ ] Backend handoff between Zap and Impetus

## Next (prioritized)

5. Provider credential management via Keychain API
6. Multi-turn conversation state with tool result accumulation
7. Streaming response chunking and client sync
8. Policy customization and approval UI contracts
9. Session fork/checkpoint without duplication
10. Error recovery and retry logic in agent loop
11. Parallel tool execution where safe
12. Cross-session state isolation and cleanup
13. Audit log with redacted tool arguments
14. Integration tests for full request flows
15. Performance benchmarks for event log queries
16. Documentation: architecture diagrams and API contracts
17. Security review: secret handling, sandbox escapes, policy bypass
18. Client examples: minimal TUI, read-only observer
19. Migration strategy for schema changes
20. End-to-end verification: no policy bypass, no credential leakage

---

**Completion criterion**: A task is done when it has a working vertical slice, tests, and passes relevant gates. Do not treat stubs, copied forks, or placeholder responses as complete flows.
