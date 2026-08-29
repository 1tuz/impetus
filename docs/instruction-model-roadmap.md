# Roadmap модели инструкций — исторический design proposal

> Этот документ фиксирует исходный design proposal. Его slices реализованы в
> текущем workspace; актуальный статус и границы смотри в
> [ROADMAP.md](ROADMAP.md) и [ARCHITECTURE.md](ARCHITECTURE.md).

Это design roadmap, а не активный release gate. Он не заменяет и не
не заменяет канонический [roadmap](ROADMAP.md).

## Общий контракт

Resolver принадлежит harness-у и читает только необязательные файлы текущего
workspace. Он строит детерминированную проекцию в порядке `SOUL -> project
rules -> conventions -> guides -> selected skills -> user text`. `AGENTS.md`
остаётся project rules, а существующие `SKILL.md` сохраняют совместимость как
skills.

Текст инструкций живёт только в transient provider request. Durable event
schema, SQLite, logs, exports и raw IPC prompt не получают этот текст.
Instruction declarations advisory: они не дают origin `user`, approval,
capability, sandbox scope, доступ к credential или право на execution.

## Slice 1 — чистый resolver

Добавить в `impetus-core` filesystem-only типы catalog/resolver без native GUI,
transport или provider-зависимости. Каждый объект имеет stable ID, kind, scope,
relative path, content hash и text. Resolver поддерживает path/ecosystem scope,
явный выбор skills и deterministic deduplication/order.

Cache — bounded и on-demand, с ключом relative path + content hash: изменение
одного файла перезагружает только этот entry. Токены считаются отдельными
estimate для project rules, conventions, guides и skills; это не tokenizer и не
основание менять provider limits. RAM-граница — только bounded cache и одна
resolved projection на запрос, без embeddings, DSL, package manager или
dependency solver.

## Slice 2 — transient provider integration

Сохранить пользовательский intent до resolution и расширить provider до
совместимого multi-message API. `stream_user_message` остаётся wrapper-ом для
существующих callers; direct provider получает упорядоченный transient message
list. Никакого изменения event schema, credential boundary или policy chain
`Policy -> Deny | Allow | NeedsApproval -> Sandbox -> Capability -> Execution`.

Проверки доказывают порядок сообщений, отсутствие instruction text в persisted
intent и то, что metadata вроде `requires: ssh-prod` не меняет policy или
capability outcome. Stable serialization и fixed prefix сохраняют возможность
provider prompt-cache; invalidation остаётся per-file из Slice 1.

## Slice 3 — negotiated context inspection

Добавить новую IPC capability, request/response и CLI-команду, которые показывают
live resolved references и totals estimated tokens, но не превращают это в TUI.
Новые поля доступны только при успешной negotiation; старые Unix и in-memory
clients сохраняют текущий `Incompatible` и existing request contract. Ответ
отражает live metadata/projection, не переносит instruction bodies в durable
events или raw prompt fields.

## Slice 4 — proposal-only learning

Добавить in-memory классификацию наблюдений: memory, convention, guide update
или skill improvement. Lifecycle фиксирован: `Observed -> Candidate -> Repeated
-> Validated -> Proposed -> Promoted`; для skills threshold строже, чем для
conventions.

Результат — proposal, а не запись в файловую систему. Ни skill, ни model не
создают и не меняют instruction files автоматически; классификация не влияет на
policy, approvals, sandbox, capabilities, credentials или execution.

## Критерий перехода

Каждый slice поставляется независимо с целевыми тестами. Перед объединением
Rust-изменений применяются repository checks из `task verify`; документация
остаётся отдельным подготовительным результатом и не объявляет v0.6 завершённой.
