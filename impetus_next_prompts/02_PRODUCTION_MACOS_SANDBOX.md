# Довести macOS sandbox до реального fail-closed исполнения

Проверь не наличие типов/Policy, а реальный путь запуска каждого agent-controlled child process.

Исторический риск: логический `Sandbox`/admission мог существовать отдельно от фактического OS confinement. Нельзя считать задачу закрытой, пока команда, прошедшая approval, физически запускается под ограничениями ОС.

## Цель

На macOS agent-controlled shell/process должен исполняться внутри production-grade sandbox boundary. Сделай абстракцию backend-а, чтобы Seatbelt-specific код не проникал в Tool Orchestrator/Policy.

Предпочтительная форма:
- `SandboxProvider` / эквивалентный контракт;
- macOS implementation через доступный системный Seatbelt path;
- fail-closed: если required sandbox недоступен или профиль не построен, опасное исполнение не запускается;
- backend можно заменить позже без переписывания execution pipeline.

## Обязательные ограничения

Проверь и реализуй по необходимости:
- deny-by-default профиль;
- workspace read;
- write только в явно разрешённые writable roots;
- отдельный session temp dir;
- запрет чтения `.ssh`, Keychain/browser profiles/других чувствительных частей `$HOME`;
- сеть deny-by-default, если capability явно не разрешает сеть;
- child processes наследуют ограничения;
- очищенный environment, без неявной передачи secrets;
- timeout;
- process group / tree termination, чтобы kill не оставлял потомков;
- bounded stdout/stderr;
- корректная обработка symlink/canonical path;
- durable события о sandbox decision без утечки секретов.

Не путай Policy и OS sandbox: Policy решает «можно ли», sandbox физически ограничивает процесс даже при ошибке в higher-level коде.

## Tests

Добавь macOS integration tests, которые доказывают:
- разрешённый write внутри workspace проходит;
- write вне разрешённого root блокируется ОС;
- чтение чувствительного пути блокируется;
- запрещённая сеть блокируется, если применимо;
- дочерний процесс не обходит sandbox;
- sandbox unavailable → fail closed;
- cancellation/timeout убивает дерево процесса.

## Acceptance criteria

Нельзя закрывать issue только на основании unit test admission logic. Нужен реальный execution smoke/integration path на macOS и отсутствие прямого unsandboxed обхода для agent-origin shell/process.

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
