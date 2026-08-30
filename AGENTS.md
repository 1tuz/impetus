# Правила для coding-агентов

Стиль (caveman), YAGNI/ponytail, **RTK** и снижение токенов — в Codewhale constitution / `~/.codewhale/RTK.md`:

- глобально: `~/.codewhale/constitution.json` + `append_system_prompt` (RTK каждый shell)
- репо: `.codewhale/constitution.json`
- **каждый `bash`:** только через `rtk …` (`rtk cargo`, `rtk git`, `rtk rg`, …)

**Субагенты (CodeWhale):** только по явному запросу или явной выгоде; cap ≤5, **не более 2 builder одновременно**. На spawn сразу: `worktree: true`, полный `write_roots` (если трогаешь `Cargo.toml`/tests — включи корень crate, не только `src/…`), один узкий slice на child. **`task verify` — один раз parent'ом**, не в каждом child. При `wall_time_budget` / API error — checkpoint + re-dispatch одного worker, не пачка из 5.

Здесь только продуктовые границы и проверка этого репо.

## Неподвижные границы

- Harness-first: текущий этап — standalone Rust runtime и CLI. Standalone TUI — first-class planned client; не начинать собственный PTY/ANSI terminal emulator без зафиксированного неудовлетворённого требования.
- Zap использует собственный UI и подключает Impetus как agent backend; отдельный adapter или личный fork допустимы. Не копировать Zap/Warp client internals внутрь harness core.
- `impetus-core` и headless runtime не зависят от terminal renderer, native GUI или конкретного клиента.
- Клиент не владеет SQLite connection, секретами, SSH transport или policy. Он отправляет typed request и отображает durable events/approvals harness-а.
- Каждый typed action имеет `origin=user|agent` и проходит `Policy → Deny | Allow | NeedsApproval`; только `Allow` либо принятое человеком approval продолжаются через `Sandbox → Capability → Execution`. Модель не может выдать себе `origin=user` или approval.
- Секреты хранятся только в macOS Keychain. В SQLite, JSONL, tracing, typed payloads и тестах — лишь reference-метки, никогда token/private key/passphrase.
- Не использовать `latest` и непинованные git dependency.

## Harness и клиентский протокол

- Controlled shell/process/PTY — capability исполнения. ANSI parser, tabs, scrollback и terminal renderer — клиентская функция; эти понятия не смешивать.
- Versioned local IPC обязан поддерживать capability negotiation, prompt/stream/status/cancel, typed approvals/diffs и явный `Incompatible` state.
- Disconnect или crash клиента не должен уничтожать durable session либо выдавать неизвестный outcome за `Completed`.
- Базовый Zap path — собственный UI Zap с подключённым Impetus backend. Structured integration строится отдельным adapter/fork; OSC/notification hooks не заменяют typed protocol.
- Local HTTP UI, Electron/WebView и Node runtime не добавлять в harness. Состав отдельного личного Zap fork не расширяет dependency/trust boundary harness-а.

## ACP и модели

- ACP — протокол между клиентом и внешним coding-agent, а не универсальный provider API и не хранилище авторизации.
- Для ACP backend авторизация принадлежит выбранному agent CLI; приложение запускает его только после явного user action и отображает его профиль/статус.
- `agent-client-protocol = 2.x` означает major Rust SDK crate; draft protocol v2 feature не включать без отдельного RFC и compatibility tests.
- Для direct provider auth использовать ровно один из вариантов: Keychain API-key reference, system-browser OAuth или local/no-secret. Никакого поля raw token в клиенте и никакой передачи секрета модели.
- URL-mode OAuth открывается только с подтверждением пользователя в системном браузере; URL виден целиком. Не использовать WebView.
- Поддерживаемость конкретного Codex/Claude/Cursor/Gemini/Qwen backend определяется установленной версией и ACP registry/discovery, не предположением о CLI-флаге.

## Проверка

После Rust-изменения обязательно выполнить:

```zsh
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Для изменений harness/provider/ACP/auth добавить тест без секрета: stream/cancel/restart, profile validation, policy decision и redaction/export.

`task verify` является коротким эквивалентом четырёх обязательных Rust-команд. `task setup` проверяет окружение и подключает repository-owned hooks.

## GitLab CI

- `.gitlab-ci.yml` — versioned contract проверки. При изменении Rust-пакетов, test/verify команд, toolchain, dependencies или CI-образа сверить затронутые jobs и актуализировать pipeline в том же изменении.
- До handoff Rust/CI-изменения выполнить `task verify` (локально). Для проверки CI: `gitlab-ci-local --stage verify` (займёт ~3-4 минуты с Docker overhead).
- **CI test scope:** `cargo test --lib --bins` (unit tests только). Integration tests из `crates/*/tests/` исключены — они требуют macOS Seatbelt, нативного окружения и долго компилируются в Docker. Локально запускать полный `task verify` с integration tests.
- При изменении `Cargo.toml` или `Cargo.lock` выполнить `task security`; RustSec/CVE, license/source/bans findings не игнорировать без versioned записи в `deny.toml` с конкретной причиной.
- Если `gitlab-ci-local` зависает >5 минут — проверить `timeout` в job definition и scope тестов (возможно, добавлены новые долгие integration tests).

## Git и коммиты

### Feature branch workflow (строго обязательно)

**НИКОГДА НЕ ПУШИТЬ НАПРЯМУЮ В `main`.** Любой push в main без MR — нарушение workflow.

**Проверка текущей ветки:** перед началом работы всегда выполнить `git branch --show-current` и убедиться, что не на `main`. Задачи из TODO.md берутся последовательно; каждая задача = один issue + одна feature branch.

#### Перед началом работы

1. **Проверить текущую ветку:** `git branch --show-current` — если `main`, остановиться и создать feature branch
2. **Проверить открытые issue:** `glab issue list` — выбрать следующую задачу из TODO.md
3. **Создать issue,** если не существует (каждая задача из TODO.md = issue)
4. **Создать feature branch от актуального main:**
   ```bash
   git checkout main
   git pull origin main
   git checkout -b feature/issue-42-short-description
   ```
   Шаблон: `feature/issue-N-description` или `fix/issue-N-bug-name`

#### Workflow

1. Работа в feature branch (никогда не в `main`)
2. Атомарные коммиты: каждый с `closes #N`, `fixes #N` или `refs #N`
3. **До push:** обязательно `task verify` (fmt, test, check, clippy)
4. **Push в feature branch:**
   ```bash
   git push -u origin feature/issue-42-short-description
   ```
5. **Создать MR через CLI:**
   ```bash
   glab mr create --fill --remove-source-branch
   ```
   Или через GitLab Web UI с галочкой «Delete source branch after merge»
6. **Включить auto-merge** в MR: «Set to auto-merge» после создания
7. CI проходит (fmt, test, check, clippy) → **GitLab автоматически мерджит в main**
8. После мерджа: `git checkout main && git pull` для следующей задачи

#### Auto-merge настройка (один раз на проект)

В GitLab Project Settings → Merge Requests:
- ✓ «Pipelines must succeed» (требовать проходящий CI)
- ✓ «All threads must be resolved» (опционально)
- Approvals: 0 (разрешить auto-merge без review)

В каждом MR нажать кнопку **«Set to auto-merge»** — мердж произойдёт после успешного pipeline.

### Commit правила

- Делить работу на атомарные коммиты по одной причине изменения; не смешивать tooling, продуктовый код и независимую документацию без необходимости.
- До commit выполнить `task verify`. Для Rust/CI-изменения при наличии `.gitlab-ci.yml` также выполнить `task ci:list` и relevant local job либо `task ci:local`; при изменении job/toolchain/dependency policy актуализировать pipeline в том же commit. Для docs-only изменения дополнительно проверить ссылки/диаграммы применимым локальным validator-ом.
- **Commit message на английском языке.** Формат: `type: Brief summary (closes #N)` или `type(scope): Summary (refs #N)`
- Разрешённые типы: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`
- Subject <= 72 символа, начинается с lowercase (после `type:`), без точки в конце
- **Issue-driven workflow (строго обязательно):** каждый commit должен ссылаться на issue через `closes #N`, `fixes #N` или `refs #N`. Если issue нет — **остановиться и создать issue сначала**. Работа без issue запрещена.
- Body (опционально) описывает «что» и «почему», не «как». Wrap на 72 символа.
- Примеры:
  - `feat: add subsystem health probes to doctor (closes #42)`
  - `fix(ipc): handle large enum variants with Box (refs #38)`
  - `docs: update implementation history for phase 2 (refs #15)`
- Не использовать `--no-verify`, не коммитить secrets, `.env`, локальные БД, provider credentials, browser caches, `target/` и generated runtime state.
- Не делать amend/rebase/force-push и не настраивать remote без прямого указания пользователя.

## Запрещённые файлы и директории в репозитории

Следующие категории файлов и директорий **запрещены** в коммитах и должны быть в `.gitignore`:

- **Build artifacts:** `target/`, `**/target/`, любые compiled binaries и intermediate build outputs
- **Temporary configs:** `config/` с example/template конфигами (допустимы только versioned `.example` файлы в `docs/` или корне)
- **Archived/obsolete docs:** `docs/archived/`, `docs/superpowers/`, historical audits/spikes/roadmaps (актуальные: `ARCHITECTURE.md`, `ROADMAP.md`)
- **Generated HTML/diagrams:** `*.html` в корне или `docs/` (кроме явно versioned reference docs)
- **IDE/tool artifacts:** `opencode.json`, `.DS_Store`, `__pycache__/`, `*.pyc`
- **Runtime state:** `*.db`, `*.db-shm`, `*.db-wal`, session logs, trace dumps

Перед коммитом проверять `git status` и `git diff --cached`. Если случайно staged запрещённый файл — `git reset HEAD <file>` и добавить в `.gitignore`.
