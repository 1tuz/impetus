# VimTrap Implementation Plan

**Issue:** #63  
**Goal:** Maximum replaceability + Minimum required configuration

## Current State

Существующие компоненты:
- `ModuleRegistry`: lifecycle, health, compatibility
- `ServiceRegistry`: agent_loop, scheduler (partial)
- `ModuleDescriptor`: id, kind, capabilities, permissions
- Traits: `AgentLoopStrategy`, `AgentScheduler`
- Module IPC, lifecycle, fallback policies

**Gap:** нет явного разделения Kernel vs Replaceable Services, нет preset model, нет declarative binding.

## Architecture Changes

### 1. Kernel Invariants (non-replaceable)

Фиксированный pipeline безопасности:

```
Action origin (user|agent)
  ↓
Policy admission
  ↓
Deny | Allow | NeedsApproval
  ↓
Sandbox capability
  ↓
Execution
  ↓
Durable outcome/event
```

**Kernel ownership:**
- `PolicyEngine` admission logic
- `ApprovalResolver` human gate
- Secrets boundary (Keychain only)
- `UnknownOutcome` semantics
- Permission enforcement
- Module isolation
- Protocol compatibility

**Non-negotiable:** custom provider не может bypass policy/sandbox/approval.

### 2. Replaceable Services

Target service contracts (постепенная миграция):

```rust
// Core services
AgentLoopStrategy       ✓ exists
AgentScheduler          ✓ exists
ModelRouter             ○ planned
ContextService          ○ planned
ReferenceService        ○ planned
MemoryService           ○ planned

// Tool & execution
ToolProvider            ○ planned (exists as ModuleKind)
SearchBackend           ✓ exists (ModuleKind)
BrowserProvider         ✓ exists (ModuleKind)

// Security & policy
CredentialResolver      ✓ exists (ModuleKind)
PolicyExtension         ✓ exists (ModuleKind)

// Output & intelligence
OutputReducer           ○ planned
RepoIntelligence        ○ planned
```

### 3. Provider Model

Слой абстракции для consumer:

```rust
pub enum ServiceProvider<T> {
    Builtin(T),
    Custom(Box<dyn ServiceTrait>),
    External(ExternalModuleHandle),
}

// Consumer depends on trait, not concrete impl
impl Harness {
    context_service: ServiceProvider<ContextService>,
    model_router: ServiceProvider<ModelRouter>,
    // ...
}
```

**Принцип:** `AgentLoop` зависит от `ContextService` trait, не от `BuiltinContextOptimizer`.

### 4. Profile System

Три preset режима:

#### Standard (default)
```yaml
# zero-config, implicit
agent_loop: builtin
scheduler: builtin
model_router: balanced
context: builtin-lazy
references: yaml
memory: builtin
policy: standard
tools: builtin
output_reducer: builtin
```

Команда: `impetus` — запускается без конфигурации.

#### Minimal
```yaml
# debugging, benchmarks, tests
agent_loop: minimal
scheduler: sync
model_router: direct
context: disabled
references: disabled
memory: disabled
policy: permissive
tools: minimal
```

Команда: `impetus --profile minimal`

#### Creator
```yaml
# advanced customization visible
# service replacement enabled
# introspection tools active
```

Команда: `impetus --profile creator`

### 5. Declarative Service Binding

Пользователь может override в `~/.config/impetus/config.yaml`:

```yaml
profile: standard

services:
  context: my-context-provider  # custom provider
  model_router: local-first     # builtin variant
  reference: company-ref        # external module

modules:
  my-context-provider:
    path: ~/.impetus/modules/my-context
    permissions:
      filesystem: [read]
```

Или для project-local override в `.impetus/config.yaml`.

### 6. Progressive Disclosure

Standard mode UI (minimal surface):
```
Model: claude-3.5-sonnet
Mode: standard
Session: abc123
Usage: 45K tokens
```

Creator mode UI (introspection):
```
Profile: creator
Providers:
  ContextService    → builtin-context
  ReferenceService  → yaml-reference
  ModelRouter       → balanced-router
  ToolProvider      → builtin-tools

Modules: 3 loaded, 1 degraded
Health: OK
```

### 7. Anti-Pattern: Hook Hell

**Запрещено:**
```rust
// Bad: hook accumulation
before_prompt, after_prompt,
before_model, after_model,
before_tool, after_tool,
before_context, after_context,
...
```

**Правильно:**
```rust
// Good: service replacement
impl ContextService for MyContextService {
    async fn optimize(&self, context: Context) -> Result<Context> {
        // Full control over logic
    }
}
```

Hooks допустимы только для proven use case, который невозможно выразить через service contract.

### 8. Fallback & Health

Использовать существующие механизмы:
- `ModuleState`: Degraded, Failed
- `FallbackPolicy`: FailFast, Retry, Alternate, Degrade, SafeDefault
- `CapabilityProbe`
- `ModuleHealth`

Пример:
```
custom ContextService failed
  ↓
fallback → builtin ContextService (if replay-safe)
  ↓
emit warning, continue with degraded state
```

Для non-replayable operations: соблюдать `UnknownOutcome`.

### 9. Introspection

Расширить существующий `impetus components`:

```bash
impetus components list         # existing
impetus components status       # existing
impetus profile show            # planned
impetus components bindings     # planned
```

Output для `bindings`:
```
ContextService     → builtin-context (v0.1.0)
ReferenceService   → yaml-reference (v0.1.0)
ModelRouter        → balanced-router (v0.1.0)
ToolProvider       → builtin-tools (v0.1.0)
SearchBackend      → builtin-search (v0.1.0)
```

## Implementation Phases

### Phase 1: Foundation (this PR)

1. **Документация границ:**
   - Создать `docs/KERNEL_INVARIANTS.md`
   - Зафиксировать non-bypassable pipeline
   - Обновить ARCHITECTURE.md

2. **Profile model:**
   - Добавить `ProfileDescriptor` enum: Standard | Minimal | Creator
   - Добавить `ProfileConfig` struct с service bindings
   - Loader для profile из config

3. **Service provider abstraction:**
   - Обобщить `ServiceProvider<T>` enum
   - Миграция одного существующего service как proof (AgentLoopStrategy?)

4. **Declarative binding:**
   - Парсинг `services:` section из config
   - Связывание builtin/custom/external providers

5. **Introspection:**
   - `impetus profile show` command
   - `impetus components bindings` command

6. **Tests:**
   - Profile loading & resolution
   - Service provider selection
   - Fallback when custom provider fails

7. **Documentation:**
   - Update ARCHITECTURE.md
   - Update ROADMAP.md
   - Add VimTrap principle to docs

### Phase 2+: Incremental Migration (future PRs)

- Migrate existing services к provider model
- Expand service contracts (ModelRouter, Context, Memory, etc)
- Enhanced introspection UI
- Creator mode tooling

## Success Criteria

После Phase 1:

**Обычный пользователь:**
```bash
impetus  # zero-config, просто работает
```

**Продвинутый пользователь:**
```bash
impetus --profile creator
impetus profile show
impetus components bindings

# declarative override в ~/.config/impetus/config.yaml
```

**Architecture:**
- Kernel invariants явно зафиксированы
- Хотя бы один service через provider model
- Profile system работает
- Introspection commands работают
- Tests покрывают основные сценарии

## Non-Goals (не в этом PR)

- Полная миграция всех services
- Marketplace/registry
- Dynamic ABI/scripting
- Сотни hooks
- GUI editor
- Сложный dependency solver

## References

- Issue #63
- VimTrap.md
- ARCHITECTURE.md
- TODO.md
- docs/ROADMAP.md
