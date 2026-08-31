# ACP: production hardening, встроенная UX-функция с изолированным service/crate

ACP для пользователя должен ощущаться встроенной возможностью Impetus, но код должен оставаться изолированным service/crate и запускать внешние agent CLI лениво, только при использовании.

Сначала проверь существующие GitHub Issue/PR. На момент подготовки этого prompt существовал PR #67 по ACP; если он всё ещё открыт, не создавай дублирующий ACP PR — продолжи/исправь существующий scope или создай follow-up issue только для независимого остатка.

## Архитектурная цель

```text
impetusd
  └─ ACP service
       └─ ACP gateway crate
            ├─ Codex external agent
            ├─ Claude external agent
            └─ Cursor external agent
```

Не встраивай Codex/Claude/Cursor runtime внутрь core. Не держи внешние агенты запущенными без необходимости. Не добавляй Cargo feature flag только ради нескольких мегабайт без benchmark evidence.

## Обязательные production gaps для проверки

Проверь текущую реализацию на:
- official Rust ACP SDK;
- корректное понимание SDK crate major vs ACP protocol version;
- `command + args + non-secret env` в agent profile;
- ACP Registry/discovery/version probing;
- явный выбор auth method пользователем, а не `auth_methods.first()`;
- system-browser login/device-code/terminal-owned auth;
- никакого сбора provider password внутри Impetus;
- Cursor native ACP path, если установленная версия его предоставляет;
- реальный `cancel` до external agent;
- permission request → Impetus Policy/approval → ACP response;
- streaming;
- health/status;
- restart/disconnect semantics;
- redaction;
- детерминированные mock integration tests;
- live smoke как дополнительный, но не единственный доказательный слой.

## Auth boundary

ACP backend владеет своей авторизацией. Impetus хранит только profile/path/version/status и безопасные references, но не raw provider token/password.

Не обещай consumer-subscription browser auth там, где конкретный внешний agent/SDK этого реально не поддерживает.

## Acceptance criteria

- Не выбирается API-key auth молча, если есть browser login.
- `profile.args` действительно доходит до spawned agent.
- Cancel реально прерывает external ACP session.
- Permissions проходят Policy, а не hard-deny/hard-allow shortcut.
- Tests покрывают stream/cancel/restart/profile validation/policy/redaction.
- PR не merge, пока functional acceptance criteria не доказаны, даже если CI зелёный.

## Обязательный workflow для этой задачи

Репозиторий: `https://github.com/1tuz/impetus`.

Работай автономно и доводи этот slice до merge. Не останавливайся после написания кода.

1. Сначала прочитай актуальные `AGENTS.md`, `TODO.md`, `ARCHITECTURE.md`, `docs/ROADMAP.md` и связанные с задачей исходники/тесты из свежего `main`.
2. Не доверяй старым аудитам и чекбоксам `[x]`: проверь реальную вертикаль от публичного API до исполнения и тестов. Если задача уже полностью реализована, не создавай дублирующий issue — зафиксируй конкретные доказательства и переходи к следующему prompt.
3. Проверь существующие GitHub Issues и Pull Requests. Если уже есть issue/PR с тем же scope, продолжай его вместо создания дубля.
4. Если gap существует:
   - создай **GitHub Issue** с acceptance criteria;
   - обнови локальный `main`;
   - создай отдельную ветку от свежего `main`: `feature/issue-N-short-name` или `fix/issue-N-short-name`;
   - никогда не коммить и не push напрямую в `main`.
5. Каждый commit должен ссылаться на issue (`refs #N`, `closes #N` или `fixes #N`) и соблюдать текущий `AGENTS.md`.
6. Если выполняешь shell-команды, соблюдай текущие repository rules, включая RTK-обёртку, если она всё ещё обязательна.
7. Это явно **GitHub workflow**: GitHub Issue → branch → GitHub Pull Request → GitHub Actions → auto-merge. Не переключай эту делегированную задачу на GitLab MR.
8. Перед push обязательно выполни полный repository verify. Для Rust минимум:
   - `cargo fmt --all -- --check`
   - `cargo test --workspace`
   - `cargo check --workspace`
   - `cargo clippy --workspace --all-targets -- -D warnings`
   - либо актуальный `task verify`, если он действительно эквивалентен этим проверкам.
9. Если менялись зависимости, дополнительно выполни актуальную security/dependency проверку проекта.
10. Создай PR с ясным описанием проблемы, решения, рисков и тестов. Включи auto-merge, если GitHub/repository rules это позволяют.
11. Следи за GitHub Actions **для точного head SHA**. Если Format / Check / Test / Clippy или другие required checks падают — исправляй в той же ветке и повторно проверяй новый head SHA.
12. Не считай задачу завершённой при `pending`, `failure`, конфликтах или непроверенном diff.
13. Merge разрешён только когда PR mergeable, required checks зелёные и acceptance criteria реально выполнены. Если auto-merge недоступен, выполни обычный merge после green checks.
14. После merge обнови локальный `main` и убедись, что issue закрыт/связан с merged PR.
15. Не смешивай в один PR независимые задачи из других prompt-файлов.

### Неподвижные архитектурные границы

- `impetusd` остаётся владельцем durable runtime/state; клиенты тонкие.
- Клиент не получает прямое владение SQLite, policy, secrets, SSH transport или runtime state.
- Все действия агента проходят `Policy → Sandbox → Capability → Execution`.
- Секреты — только через opaque references/system credential store; raw token/private key/passphrase не писать в SQLite, events, logs или tests.
- Не тащи Electron/WebView/Node/Chromium как обязательную зависимость core.
- Не переписывай проект с нуля и не ломай Event Log / durable session architecture.
- Не расширяй permissions автоматически ради удобства.
