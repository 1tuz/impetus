# Реализовать Context Optimizer как реальный prompt-building слой

Нужен не набор структур, а вертикаль, реально используемая перед каждым model call.

## Цель

Собирать минимальный релевантный контекст в пределах token budget и не тащить всю durable history/tools/artifacts в каждый prompt.

Реализуй/доведи:
- детерминированный stable prefix;
- prompt-cache-friendly порядок постоянных инструкций и tool schemas;
- HOT / WARM / COLD tiers;
- token-budgeted selection;
- lazy tool/module/MCP/instruction descriptions;
- delta context вместо повторной передачи полного состояния;
- telemetry: сколько токенов было доступно, отобрано, отброшено, восстановлено из artifact;
- deterministic reducers;
- large artifacts остаются `ArtifactRef`, а не вставляются целиком;
- чтение крупных artifact частями с bounded summarization;
- task-aware instruction/skill selection без расширения permissions.

## Требования к tiering

HOT:
- текущая задача;
- недавние сообщения;
- активный plan/run;
- pending approvals;
- последние релевантные tool observations/diff.

WARM:
- краткие summaries недавней работы;
- релевантные решения/ошибки/symbols;
- недавние, но не обязательные файлы/observations.

COLD:
- полный старый event history;
- большие logs;
- старые diffs;
- web pages/raw outputs;
- большие source/artifact bodies.

COLD должен быть доступен по ссылке/поиску, но не занимать prompt без причины.

## Tool selection

Не отправляй модели все tools. Для конкретной задачи выбирай только релевантный bounded set. Модель должна иметь способ запросить/обнаружить дополнительный tool family без постоянной загрузки всех schemas.

## Acceptance criteria

- AgentLoop реально использует Context Optimizer.
- Есть детерминированные tests на одинаковый порядок/prefix.
- Есть tests на budget overflow и корректное отбрасывание COLD/WARM.
- Есть tests, что нужный tool/instruction можно lazy-load.
- Есть измеримые token savings на synthetic long session.
- Optimizer не получает authority обходить Policy/permissions.

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
