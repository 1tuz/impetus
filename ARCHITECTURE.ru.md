# Архитектура Impetus

Canonical архитектурный контракт. Отделяет **текущий код** от **product target**.

| Документ | Роль |
| --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Инварианты, границы, ownership, module model |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Фазы и gates |
| [TODO.md](TODO.md) | Исполнимые задачи |
| [docs/IMPLEMENTATION_HISTORY.md](docs/IMPLEMENTATION_HISTORY.md) | Только завершённые slices |

## Product invariant

Impetus — terminal-first, local-first, all-in-one Agent Harness for Engineering:
durable sessions/events, agent/tool orchestration, safety, credentials и
execution authority за заменяемыми client surfaces.

## Binary topology

```text
impetus       = user-facing CLI / future TUI
impetusd      = authoritative daemon / runtime
impetus-core  = domain / runtime libraries (no standalone binary)
```

Целевая схема:

```text
User
  ↓
impetus (CLI / TUI)
  ↓
HarnessClient
  ↓
versioned Unix Domain Socket IPC
  ↓
impetusd
  ↓
runtime / sessions / models / tools / safety / storage
```

`impetusd` — единственный authoritative owner:

- durable sessions;
- Event Log;
- SQLite;
- Agent Runtime;
- Model Router;
- Tool Runtime;
- policy;
- approvals;
- sandbox;
- execution;
- artifacts;
- context;
- memory;
- swarm;
- credential references.

Клиенты не владеют authoritative state: ни SQLite, ни policy, ни model/tool
runtime, ни секретами, ни session authority.

**Текущий gap.** Crate split `impetus` / `impetusd` / `impetus-core` в коде
есть; часть docs и dev-tooling ещё ссылается на старую схему, где `impetus`
был daemon. Полная миграция — см. [TODO.md](TODO.md) § Binary topology.

## System map (target)

```text
                         Clients
              ┌───────────┼────────────┐
              │           │            │
           impetus       Zap        future
         (CLI/TUI)    (adapter)    (remote)
              │           │            │
              └──── HarnessClient ─────┘
                          │
                    versioned IPC
                          │
                      impetusd
                          │
                 ┌────────▼─────────┐
                 │  Harness Kernel  │
                 │                  │
                 │ durable state    │
                 │ safety boundary  │
                 │ event authority  │
                 └────────┬─────────┘
                          │
                 Module / Service Runtime
                          │
       ┌──────────────────┼─────────────────────┐
       │                  │                     │
     Models             Agents                Tools
       │                  │                     │
       ├── Context / Repo Intelligence          │
       ├── CI / SCM                             │
       ├── Remote                               │
       ├── Web / Internet Research              │
       ├── Output Optimization                  │
       ├── Memory                               │
       └── Extension Compatibility              │
```

Снаружи Module Runtime остаётся mandatory safety/durability boundary. Ни один
client, plugin или module не обходит Harness Kernel.

## Workspace layout

```text
impetus/
├── crates/
│   ├── impetus-core/          durable domain/runtime foundation
│   ├── impetusd/              headless daemon + Unix socket server
│   ├── impetus/               user-facing CLI (target: CLI/TUI)
│   ├── impetus-client/        HarnessClient + local transports
│   ├── impetus-cli/           legacy reference client (deprecated)
│   ├── impetus-zap-adapter/   experimental historical integration baseline
│   └── impetus-acp-gateway/   ACP gateway library
├── config/                    capability and provider configuration
└── docs/                      contracts, roadmap, history
```

## CURRENT (code today)

```text
impetus CLI ──HarnessClient──► impetusd ──► impetus-core
Zap adapter ──HarnessClient──┘
```

Реализовано: durable events, policy/approval, versioned Unix-socket protocol,
`HarnessClient`, provider registry foundation, copied-event forks, command/JSON
client, attachment/diff/detail DTOs with **bounded ephemeral/in-memory** backing.
Agent Loop / Tool Orchestrator — **skeleton only** (`extract_tool_calls()` and
tool execution still placeholder). Zap adapter — experimental baseline, не target
integration architecture.

**Не реализовано:** durable `ArtifactStore`, working autonomous agent loop,
Web / Internet Research subsystem, standalone TUI, `impetus doctor`, Module Runtime,
полный Session DAG, model router, remote agent flow end-to-end, extension
compatibility layer, `impetus components`.

## Harness Kernel — неподвижные инварианты

Даже при высокой модульности нельзя позволять заменить или обойти:

```text
origin=user|agent
  ↓
Policy → Deny | Allow | NeedsApproval
  ↓
Sandbox
  ↓
Capability
  ↓
Execution
  ↓
Durable Observation / Event
```

Также invariant:

- durable session authority в `impetusd`;
- durable Event Log semantics;
- approval integrity и `ActionFingerprint` / exact effect;
- secret isolation (platform credential store + opaque references elsewhere; see Credentials);
- unknown-outcome semantics (disconnect/crash ≠ `Completed`);
- capability admission.

Никакой plugin/module не выполняет privileged action в обход этого pipeline.
Модель не может выдать себе `origin=user` или approval.

Секреты — только platform credential store (см. Credentials). SQLite, JSONL,
tracing, events, tests — opaque references, never raw tokens/keys/passphrases.

### Credentials (by platform)

| Platform | Secret store | In profiles / SQLite / events |
| --- | --- | --- |
| macOS | Keychain | opaque `service` / `account` references only |
| Linux (target) | system credential store (e.g. libsecret / portal — TBD per distro) | same: references only |
| local / no-secret | none | loopback endpoints only |

Raw tokens, private keys, and passphrases never belong in SQLite, config, JSONL,
tracing, or event payloads on any OS.

## Заменяемые реализации

Через Module Runtime постепенно заменяемы:

- model providers;
- local model runtimes;
- external agent adapters (ACP);
- tools;
- MCP;
- output optimizers (включая RTK);
- tokenizers;
- context reducers;
- Repo Intelligence;
- Tree-sitter parsers;
- LSP;
- memory backends;
- CI backends;
- SCM/Git integrations;
- sandbox implementations;
- SSH/tmux/SFTP implementations;
- storage backend;
- artifact storage (durable `ArtifactStore`; current attachment backing is ephemeral/in-memory);
- `AgentLoopStrategy` / `AgentScheduler` (orchestration policy only — not safety pipeline);
- `SearchBackend` / `BrowserProvider` (web research; see Web / Internet Research);
- TUI/client surfaces;
- Zap adapter;
- router strategies;
- swarm policies;
- self-repair strategies.

**Заменяемость реализации ≠ заменяемость security/durability invariant.**

## Module Runtime

Фундамент настоящей модульности:

```text
Impetus Module Runtime
+
Service Registry
+
Capability Registry
```

Модули зависят от typed service contracts, не от конкретных реализаций.

```text
НЕ:  AgentLoop → RTK | OpenAI | GitLab | rust-analyzer

ДА:  AgentLoopStrategy / AgentScheduler
         ↓
     Service / Capability contract
         ↓
     Module Registry
         ↓
     selected implementation
```

`AgentLoopStrategy` and `AgentScheduler` are replaceable orchestration choices
(which tools to run, when to escalate, retry policy). They still emit typed
effects that **must** pass the Kernel pipeline unchanged; they cannot bypass
Policy, Sandbox, Capability, or durable event recording.

### ModuleDescriptor (архитектурный контракт)

Не обязан быть именно такими Rust structs сейчас; фиксирует shape:

```text
ModuleDescriptor
  id
  kind
  implementation_version
  contract_version
  provides[]
  requires[]
  capabilities[]
  compatibility: harness protocol, service contracts, platforms, external versions
  permissions: filesystem, process, network, secrets, remote
  lifecycle: discover, probe, start, health, stop
  execution_semantics: read_only | idempotent | mutating | non_replayable
  fallback_policy
  status: healthy | degraded | incompatible | unavailable
```

### Capability probing

Версия `tool >= X.Y` недостаточна. Нужна capability discovery/probing:

```text
RTK 0.x
  cargo.test          supported
  --workspace         supported
  --all-features      supported
  --nocapture         unsupported
```

То же для Codex, Claude, Gemini, Qwen, Cursor, MCP, LSP, rust-analyzer, GitLab,
GitHub, tmux, SSH, MLX, llama.cpp и других быстро меняющихся компонентов.

### Safe fallback

Каждый optional module имеет fallback policy, разрешённую только когда безопасно:

```text
Output optimization
  RTK → unsupported → Builtin reducer → unavailable → Raw bounded output + ArtifactRef
```

Execution semantics минимум:

```text
read_only | idempotent | mutating | non_replayable
```

Outcomes:

```text
NotStarted | Started | Completed | Failed | UnknownOutcome
```

**При `UnknownOutcome` нельзя автоматически повторять mutating/non-replayable
действие через другой backend** (RTK, SSH, Git, CI, cloud APIs, deployment,
external tools, plugins).

### External module isolation

Built-in modules — Rust traits in-process. Сторонние / непроверенные:

- предпочтительно отдельный process;
- versioned IPC;
- ограниченные capabilities;
- явные permissions;
- sandbox where applicable.

Не строить ecosystem на Rust dynamic libraries как основном plugin ABI.
JS/TS/Cordis/plugin runtime не входит в trust boundary `impetusd` без
adapter/bridge.

## Extension Compatibility Layer

```text
External extension ecosystem
            ↓
Compatibility Adapter
            ↓
Canonical Impetus Module / Skill representation
            ↓
Module Runtime
```

Исследовать совместимость с актуальными upstream-спецификациями:

- Agent Skills;
- MCP;
- Agent Plugins;
- Claude Code extensions/plugins;
- Codex extensions/plugins/skills;
- Cursor plugins/rules/skills/agents/commands;
- DeepSeek Harness/Cordis (через adapter, не arbitrary TS в daemon).

### Canonical internal representation

Внешние форматы нормализуются внутри, не распространяются по core:

```text
CanonicalModuleSpec | CanonicalSkill | Instruction | AgentProfile
| Command | LifecycleHook | McpModule | ToolProvider
```

Примеры:

```text
Claude/Cursor/Codex Skill  → CanonicalSkill
CLAUDE.md / AGENTS.md / Cursor rule → Instruction
external agent definition  → AgentProfile
MCP                        → McpModule
plugin command             → Command
```

### Partial compatibility

При импорте — capability matrix, не all-or-nothing:

```text
Plugin: example
  skills     native
  MCP        native
  rules      adapted
  agents     adapted
  commands   adapted
  hooks      partial
  UI         unsupported
```

Статусы: `SUPPORTED | PARTIAL | UNSUPPORTED | INCOMPATIBLE`. Unsupported
component не обязан блокировать весь package.

## Output optimization и RTK

RTK — optional Output Optimizer, не обязательный execution backend:

```text
CommandSpec
   ↓
Policy / Approval / Sandbox
   ↓
Execution
   ↓
Raw Observation
   ↓
Output Optimization
   ├─ structured parser (native observation)
   ├─ builtin reducer
   ├─ RTK (optional, probed, replaceable)
   └─ raw bounded fallback → ArtifactRef
```

Целевые native structured observations:

```text
cargo test → TestObservation
git diff   → DiffObservation
search     → SearchObservation
web        → WebObservation
CI         → PipelineObservation
```

Полный raw output — Artifact. RTK removable без изменения Agent Loop.

## Provider modularity

`ModelProvider` / `ProviderRegistry` — часть Module Runtime. Router выбирает
по capability, complexity, health, latency, cost, privacy, context, prompt cache,
budget, reasoning need. Политики: `local-first`, `free-first`, `balanced`,
`quality-first`.

Escalation (target):

```text
light/local model
  ↓ задача сложная
minimal sanitised escalation request
  ↓
strong/cloud model
  ↓
результат → local agent продолжает
```

Чувствительный repository context по умолчанию не уходит в облако.

## Context Optimizer и модули

Modules/tools/MCP/instructions не обязаны всегда грузиться в prompt:

- lazy discovery;
- lazy module/tool descriptions;
- token-budgeted selection;
- HOT/WARM/COLD;
- artifacts;
- structured observations;
- reducers;
- prompt-cache friendly stable prefix.

Модульность не раздувает system prompt сотнями descriptions.

## Clients

Все clients — thin surfaces через `HarnessClient`; без специального обхода core:

| Client | Path |
| --- | --- |
| Standalone `impetus` CLI/TUI | `HarnessClient` → IPC → `impetusd` |
| Zap | own UI + adapter/`HarnessClient` |
| Future Android/remote | `HarnessClient` |

`impetus` — не terminal emulator. PTY/ANSI/tabs/scrollback/renderer — client
concern.

### TUI strategy

JCode ([1jehuang/jcode](https://github.com/1jehuang/jcode)) — primary **UX
reference** для standalone TUI после source audit; не runtime dependency и не
fork source. Audit plan: [docs/TUI_REFERENCE.md](docs/TUI_REFERENCE.md)
(**not started**).

```text
JCode  → reference / UX patterns
Impetus → собственный thin TUI client
```

Baseline для исследования: Ratatui + Crossterm. Из JCode — presentation-only
(composer, keyboard, streaming, markdown, diff, approvals, session picker, fuzzy
search, command palette, scrolling, resize, status/usage, redraw coalescing).
Не переносить: Agent Runtime, providers, session/tool authority, auth state.

Codex — secondary UX reference (composer, large paste, doctor, approvals,
errors/remediation). Детальный audit: [docs/TUI_REFERENCE.md](docs/TUI_REFERENCE.md).

### Large paste

Обязательна поддержка terminal bracketed paste.

Многострочный paste: один prompt, LF внутри не submit, Unicode сохраняется,
CRLF/LF нормализуются. Большая вставка компактно:

```text
[Pasted text · 184 KB · 3920 lines]
```

Не через раздувание IPC JSON до мегабайт:

```text
paste → composer → large-paste detection → bounded/chunked upload
  → impetusd → ArtifactStore → ArtifactRef → Context Builder / Agent
```

Context Builder читает большой paste частями, сокращает с учётом token budget.

## Web / Internet Research

First-class Agent Runtime capability: autonomous internet research **без**
обязательных cloud search APIs, Python/Docker sidecar, или paid keys. Base path:
**native Rust + HTTP** (search HTML scraping + bounded fetch). JCode
([1jehuang/jcode](https://github.com/1jehuang/jcode)) — primary **implementation
reference** для websearch/webfetch/browser; upstream audit before locking details
(см. [TODO.md](TODO.md) § WEB / INTERNET RESEARCH). Не копировать JCode
architecture целиком; `ADAPT | REIMPLEMENT | SKIP` + attribution при переносе кода.

**Приоритет:** core harness capability, не marketplace plugin. После install —
рабочий search + page read **in perspective**, без доп. инфраструктуры.

```text
Agent Loop
   ↓
WebResearchService
   │
   ├── WebSearch
   │     └── SearchBackend contract
   │          ├── DuckDuckGoHtml   ← default/native
   │          ├── BingHtml         ← native fallback
   │          ├── SearXNG          ← optional
   │          └── future / API backends (Tavily, Exa, … — optional only)
   │
   ├── WebFetch                    ← native Rust
   │
   └── BrowserService
          └── BrowserProvider      ← optional / heavier
```

Agent Loop **не** зависит от DuckDuckGo/Bing/SearXNG напрямую — только от
contracts + Module Runtime selection. Capability probing важнее version compare.

### WebSearch

Base Impetus **не требует:** Tavily, Exa, Perplexity, Brave/Bing API keys,
Python, SearXNG daemon, Docker.

```text
DuckDuckGo HTML → failure/block → Bing HTML → failure → optional SearchBackend
```

Native Rust HTTP client. API providers — сменные optional `SearchBackend` only.

### WebFetch

Отдельный service/tool; **не смешивать** с `WebSearch`:

```text
URL → HTTP fetch → bounded response → HTML extraction → clean text/markdown
  → ArtifactStore → compact WebObservation (+ ArtifactRef if large)
```

Planned: redirects, timeout, max size, MIME, HTML→markdown, links, title,
source URL, timestamp, content hash, truncation; model gets bounded preview/chunks.

### Browser

JS-heavy sites — optional `BrowserService` → `BrowserProvider`. Reference:
JCode Browser Provider Protocol (normalized contract, capability negotiation,
replaceable Firefox/Chrome/WebDriver/Safari, health, session, snapshot, click,
type, wait, screenshot, optional eval/scroll/tabs/downloads). **Не обязателен**
для ordinary search/fetch. **Не тащить** Chromium/Playwright/Node в mandatory core.

### Module Runtime wiring

```text
WebSearchService → SearchBackend contract → selected implementation
BrowserService   → BrowserProvider contract → selected implementation
```

Search backend degradation не делает весь harness unhealthy (`degraded` + fallback).

### Network capabilities (semantic)

`NetworkConnect` alone недостаточен. Planned fine-grained capabilities:

```text
web.read | web.search | web.download | web.browser | web.submit | web.upload
```

`web.search` / `web.read` — may allow session-level policy. Outbound data
(POST, form submit, upload, authenticated action) — stricter Policy/Approval.

Всё равно через Kernel:

```text
origin → Policy → Approval? → Network/Sandbox admission → Capability → Execution → Observation
```

### SSRF / network safety

Web tools must not reach by default: localhost, `127.0.0.0/8`, `::1`, private LAN,
link-local, cloud metadata, Unix/local services. Validate initial URL, DNS,
redirect chain, final destination. LAN/internal access — **отдельная**
capability/policy, not default `web.read`.

### WebObservation

```text
WebObservation
  query | source_url
  url, title, snippet | content
  status, content_type, retrieved_at, provenance
  truncated, artifact_ref
```

Search → structured result list. Fetch → cleaned document. Raw HTML/large body →
Artifact; не бездумно в model context.

### Research loop (target)

Harness-owned flow, not model/provider magic:

```text
search → select results → fetch → extract links → follow → fetch
  → compare sources → answer with provenance/citations
```

Integrated with Agent Loop / Context Optimizer (structured obs + artifact refs).

### `impetus doctor` (web)

```text
Internet access     enabled/disabled
WebFetch            healthy | degraded | unavailable
WebSearch
  DuckDuckGo HTML   healthy | degraded
  Bing HTML         healthy | degraded
  SearXNG           unavailable | healthy
BrowserProvider     unavailable | healthy
Network policy      …
```

One search backend failing → `DEGRADED — web search fallback available`, not
global unhealthy.

## External agents и ACP

ACP — protocol для внешних coding agents, не universal provider API и не auth
store. Подключение через `ExternalAgentAdapter`: discovery, version, capability
negotiation, lifecycle, stream, cancel, reconnect, permissions; CLI-owned auth.
Поддержка backend — по installed version + ACP registry/discovery, не по имени
или неизвестному flag.

## Diagnostics

### `impetus doctor`

First-class CLI capability:

```bash
impetus doctor
impetus doctor --json
```

Диагностика через typed APIs (не shell parsing): versions, daemon discovery,
socket, IPC handshake, protocol compatibility, daemon readiness, Event Store,
SQLite/WAL/schema/migrations, Artifact Store, sandbox, policy, approvals,
platform credential store, ProviderRegistry, providers, model capabilities, tools, external
agents, optional modules, compatibility adapters, remote capabilities, **web
research** (WebFetch, SearchBackend health, BrowserProvider), disk/runtime
health.

`doctor --json`: versioned schema, redacted, bug-report ready, no raw secrets.
Remediation hints, не только errors. Partial extension compatibility matrix —
в scope doctor.

### `impetus components` (target)

User-facing introspection: list, status, versions, compatibility, source, health,
update, disable. Optional component update без полного релиза Impetus где
возможно; concept version/digest lock для reproducibility. Marketplace — не
сейчас.

## Maturity snapshot

| Area | Current | Target |
| --- | --- | --- |
| Daemon/client split | crates exist; docs/tooling gaps | clean `impetus`/`impetusd` everywhere |
| Safety pipeline | policy, approval, sandbox, admission | unchanged invariants |
| Provider | `ModelProvider`, registry foundation | router + escalation |
| Context | copied forks, compaction primitives | Session DAG, lazy modules |
| Attachments | bounded ephemeral/in-memory DTO backing | durable `ArtifactStore` |
| Agent loop | skeleton; placeholder tool path | full orchestrator + research loop |
| Web research | not implemented | native search/fetch; optional browser |
| TUI | none | thin Ratatui client |
| Module Runtime | not implemented | registry, contracts, probing |
| Extensions | not implemented | adapters + canonical model |
| RTK | dev convention (CodeWhale) | optional output optimizer module |
| Remote | models/stubs | controlled E2E flow |

Визуальный [request control flow](docs/architecture-map.html) — один safety path,
не полная system map.
