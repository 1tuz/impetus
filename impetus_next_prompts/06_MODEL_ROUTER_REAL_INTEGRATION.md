# Подключить Model Router к реальному Prompt → AgentLoop пути

Проверь, не остаётся ли `ModelRouter` отдельным хорошо протестированным объектом, в то время как production Prompt path продолжает использовать фиксированный `default_provider_id`.

## Цель

Каждый новый run должен получать осознанный provider/model selection через единый routing service.

Учитывать:
- capability requirements;
- health;
- cost;
- latency;
- privacy/locality;
- context window;
- prompt-cache suitability;
- session budget;
- reasoning/tool/vision requirements;
- явную user policy: local-first/free-first/balanced/quality-first/low-latency, если она поддерживается.

## Не делать

Не добавляй self-judging research escalation между локальной и облачной моделью.

Обычный fallback при provider failure допустим, но должен соблюдать:
- mutating/non-replayable `UnknownOutcome`;
- отсутствие автоматического повтора потенциально уже выполненного действия;
- provider/model identity без неоднозначности `model_id` между разными providers.

## Улучшения scoring

Проверь:
- не даёт ли `unknown cost` необоснованный бонус;
- не трактуется ли context size как суррогат quality;
- реально ли `max_tokens/context_limit` влияет на hard eligibility, а не только слабый score penalty;
- используются ли наблюдаемые rolling health/latency/error metrics вместо вечной статической metadata;
- есть ли budget preflight и post-call accounting.

## Acceptance criteria

- Production Prompt path реально вызывает router.
- Выбранный provider+model записываются durable/redacted event/diagnostics.
- User-selected routing policy воспроизводима.
- Routing tests покрывают одинаковые model IDs у разных providers.
- Недостаточный context/budget приводит к понятному отказу/другому допустимому кандидату.
- Provider failure fallback не нарушает UnknownOutcome.

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
