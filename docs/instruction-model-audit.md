# Аудит модели инструкций — исторический baseline

> Этот аудит описывает состояние до реализации instruction resolver. Он
> сохранён как обоснование design, а не как описание текущего runtime. За
> текущими границами обращайся к [ROADMAP.md](ROADMAP.md) и
> [ARCHITECTURE.md](ARCHITECTURE.md).

## Подтверждённый runtime-поток

Для direct OpenAI-compatible provider путь пользовательского текста сейчас
строго такой:

```text
IpcRequest::Prompt
  -> Harness::handle
  -> handle_request
  -> run_openai_stream
  -> OpenAiCompatibleProvider::stream_user_message
```

`Harness::handle` передаёт нормализованный IPC-запрос в transport-neutral
dispatcher. В ветке `IpcRequest::Prompt` harness сохраняет исходный `text` как
intent и запускает run; его копия передаётся в `run_openai_stream`. Эта функция
запрашивает credential только на время provider-вызова и вызывает
`stream_user_message`. Provider строит OpenAI-compatible `messages` из одного
сообщения с ролью `user`. Mock backend идёт отдельным `run_mock_stream` путём.

Следовательно, подтверждённого загрузчика или resolver-а инструкций между
persisted intent и transient provider request сейчас нет. В частности, runtime
не ищет `AGENTS.md`, `SKILL.md` или `.impetus/`, не строит scoped context и не
передаёт дополнительные system/user messages provider-у.

## Почему `plugins.rs` не подходит

`crates/impetus-core/src/plugins.rs` реализует реестр capability manifests:
он десериализует JSON, валидирует идентификаторы, версии, разрешения и
roadmap-фазы и хранит manifests в `BTreeMap`. Это не instruction registry:

- он не читает workspace-файлы и не хранит их текст либо content hash;
- у него нет scope matching, явных ссылок skill → guide/convention и порядка
  prompt context;
- его `permissions` — часть capability-контракта, а instruction metadata не
  может менять Policy, approvals, sandbox или доступ к секретам;
- расширение этого реестра смешало бы статические capability manifests с
  transient workspace context и нарушило бы указанную границу доверия.

Нужен отдельный harness-owned resolver с файловой моделью инструкций; он не
должен переиспользовать capability registry как хранилище или механизм
авторизации.

## Зафиксированная граница поставки

Целевой resolver работает с необязательной структурой workspace:

```text
AGENTS.md                         project rules
.impetus/SOUL.md                  identity
.impetus/conventions/*.md         declarative conventions
.impetus/guides/*.md              domain guides
.impetus/skills/<name>/SKILL.md   procedural skills
```

Контекст формируется только для активного workspace и только transiently после
сохранения исходного intent: `SOUL -> project rules -> conventions -> guides ->
selected skills -> user text`. Тела инструкций не попадают в SQLite events,
логи, exports или raw IPC prompt. Их metadata остаётся advisory: она не меняет
`ActionOrigin`, решения policy, approval, sandbox scope, capability manifest,
credentials или execution.

Подробные slices и их критерии — в
[instruction-model-roadmap.md](instruction-model-roadmap.md).
