# Direct provider auth: реальный Keychain/OAuth lifecycle

Эта задача относится только к **direct provider auth**, не к ACP agent-owned auth.

## Цель

Direct provider должен использовать один из поддержанных вариантов:
- macOS Keychain API-key reference;
- system-browser OAuth;
- local/no-secret.

Никаких raw tokens в client payloads/events/SQLite/logs.

## OAuth

Если текущий OAuth flow остаётся тестовым scaffold, доведи его:
- system browser only;
- случайный loopback callback port, а не жёстко `8080`;
- state validation;
- PKCE, если provider flow это поддерживает/требует;
- реальный code → token exchange;
- refresh lifecycle;
- expiry handling;
- cancellation/timeout;
- provider errors;
- token storage только в Keychain;
- DB хранит opaque credential reference;
- URL и callback diagnostics без token leakage.

## Keychain

Проверь корректный upsert/update существующего credential, а не только first-create path.
Удаление/rotation должны быть определены и тестируемы.

## Acceptance criteria

- Authorization code не используется как fake access token.
- Restart может восстановить auth через credential reference.
- Redacted export/doctor не содержит secret material.
- Tests покрывают state mismatch, timeout, refresh, duplicate credential update и redaction.

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
