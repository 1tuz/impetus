# Safe Auto mode

## Зачем нужен отдельный режим

`Safe Auto` уменьшает усталость от подтверждений, но не превращает приложение в
`--dangerously-skip-permissions`. Агент продолжает создавать только
`origin=agent` actions, а право на исполнение остаётся у локального runtime.
Критически опасное действие нельзя провести ни текстом модели, ни правилом из
репозитория, ни положительным ответом классификатора.

Режимы MVP:

| Режим | Поведение |
| --- | --- |
| `Manual` | безопасные read-only actions идут автоматически, остальные показывают точную approval-card |
| `Safe Auto` | безопасные actions идут автоматически; спорные проверяет отдельный safety reviewer; human-only actions ждут человека; опасные блокируются |

Режима «разрешить всё» в host-приложении нет. Переключить режим может только
человек через видимый control; модель, ACP backend, capability и workspace config
не могут этого сделать.

## Что подтверждено исследованием Claude Code

Актуальный Claude Code Auto mode использует две независимые защиты:

1. probe проверяет результаты file/web/shell/tool до попадания в контекст агента
   и помечает вероятную prompt injection;
2. отдельный transcript classifier проверяет предложенное действие до исполнения.

Классификатор не получает raw tool outputs. Он видит пользовательский intent и
tool calls, поэтому инструкция из прочитанного файла не может напрямую убедить
контролёр разрешить следующий вызов. Отдельный быстрый фильтр пропускает очевидно
безопасное, а reasoning включается только для подозрительного. Ошибка разбора или
отсутствие вердикта означает отказ. Широкие allow-правила при входе в Auto mode
отключаются, а repo-local config не может расширить auto permissions.

Это полезный архитектурный образец, но не гарантия безопасности и не контракт
совместимости с конкретной версией Claude Code. Нам не нужен восстановленный
внутренний исходник: публичные первичные материалы уже фиксируют threat model,
порядок решений и fail-closed поведение; реализация Impetus остаётся
собственной, типизированной и тестируемой.

## Канонический поток Impetus

```text
origin=agent Action
  + exact UserIntent revision
  + ScopeSnapshot
  + EnvironmentSnapshot
          │
          ▼
Deterministic Policy
  ├─ HardDeny ───────────────────────────────────────────────► Block
  ├─ SafeAllow ──────────────────────────────────────────────► Sandbox
  ├─ HumanOnly ──────────────────────────────────────────────► Approval card
  └─ AutoReviewable
          │
          ▼
Separate Safety Reviewer (Safe Auto only)
  ├─ Allow ──────────────────────────────────────────────────► Sandbox
  ├─ Block ──────────────────────────────────────────────────► no execution
  └─ timeout / unavailable / invalid verdict ────────────────► Block

Sandbox → Capability → Execution → output probe → agent context
```

В `Manual` ветка `AutoReviewable` идёт в human approval. В `Safe Auto` reviewer
никогда не меняет `HardDeny` и `HumanOnly`: он может разрешить только действие,
которое deterministic policy заранее признала допустимым для автоматической
проверки.

## Порядок решения

Первое совпадение завершает проверку:

1. Проверить origin, тип action, нормализованный target и freshness snapshots.
2. Применить managed hard-deny и встроенные invariants.
3. Применить пользовательские deny/ask правила. Repo config может только сузить
   права через deny/ask, но не добавить allow.
4. Разрешить узкий детерминированный safe set: read-only lookup и запись внутри
   workspace без protected path, symlink escape и смены trust boundary.
5. Оставить human-only действия человеку независимо от режима.
6. Только оставшиеся auto-reviewable actions передать отдельному reviewer.
7. Повторно проверить неизменность action fingerprint и snapshots перед sandbox.

Safety reviewer получает не shell transcript, а типизированный пакет:

```text
AutoReviewRequest
  action_fingerprint
  action_kind + normalized target + arguments
  user_intent_revision + user-authored boundaries
  workspace/remotes/environment snapshot hashes
  policy/rule/reviewer versions
  redacted provenance labels, never secret bytes or raw tool output
```

Вердикт — закрытый enum `Allow | Block(reason_code)`. Свободный текст модели не
парсится как permission. При смене режима, intent, target, workspace/remotes,
policy или environment pending verdict отбрасывается, а cache очищается.

## Неподвижные запреты

### Hard deny: не исполнять ни в одном режиме

- вывод, копирование или отправка credential/private key/token/passphrase;
- рекурсивное удаление системного, домашнего или неразрешённого корня;
- обход policy/sandbox, отключение аудита или запуск другого агента с bypass/no-sandbox;
- изменение собственных managed policy/auto-review правил агентом;
- выполнение из symlink/path escape за workspace scope;
- отправка файла или terminal output во внешний target без явного outbound scope.

### Human-only: Safe Auto никогда не разрешает сам

- production deploy/migration, destructive IaC apply/destroy;
- IAM/repository grant, secret-manager write, DNS/TLS change;
- force push, destructive git reset/clean/stash над несохранённой работой;
- merge/approve без требуемого human review, выключение CI protection;
- массовое удаление cloud/shared resources, cluster-wide Kubernetes effect;
- новый/repointed remote, неизвестный SSH host, public tunnel/port-forward;
- публикация или upload данных за текущую trust boundary;
- отправка модели нового файла/скриншота, не выбранного человеком для этого prompt.

Human-only не означает «безопасно после одного клика». Approval-card показывает
точный command/diff/target, scope и причину; policy всё равно может вернуть
`Deny`. У classifier block нет кнопки мгновенного «всё равно запусти»: человек
переходит в `Manual`, формулирует точный intent заново и получает обычную policy
проверку.

## Prompt injection и недоверенный ввод

Любой file, web response, terminal output, MCP/ACP result и attachment —
недоверенный контент. До передачи основному агенту input probe возвращает
`Clean | Suspicious | Blocked`; suspicious content получает provenance label и
видимое предупреждение. Reviewer не получает raw bytes этого контента. Сам probe
не выдаёт permission и при недоступности не расширяет права.

Первый MVP reviewer должен проверять все shell/process actions, а не полагаться на
широкий allow pattern. Детерминированные read-only operations могут миновать
модельный reviewer, но не policy и scope validation.

## Отказобезопасность и аудит

- timeout, network failure, unavailable model, invalid schema или empty verdict →
  `Block`, никогда `Allow`;
- после 3 последовательных либо 20 суммарных classifier blocks `Safe Auto`
  ставится на паузу и возвращается в `Manual`; blocked action не запускается;
- non-interactive/background run не может сам подтвердить fallback;
- один action fingerprint допускает один effect; retry создаёт новый attempt ID;
- cache key включает action, intent, scope/environment и версии правил;
- mode switch и любое изменение ключа инвалидируют cache и pending checks.

Durable events без prompt/секретов:

- `AutoModeChanged` — только user origin;
- `AutoReviewRequested`;
- `AutoReviewAllowed` / `AutoReviewBlocked`;
- `AutoModePaused`;
- `InputProbeFlagged`.

Событие хранит attempt/action fingerprint, версии policy/reviewer, snapshot refs,
latency и стабильный reason code. Полный classifier prompt, chain-of-thought,
secret-bearing output и attachment bytes в SQLite/tracing/export не попадают.

## Этапы реализации

- v0.3: mode state/control, typed review request/verdict, mock reviewer, event
  projection и input-provenance contract без host effects;
- v0.5: enforcement перед реальными local effects, cache invalidation, pause
  thresholds, sandbox integration и adversarial suite;
- v0.6: отдельные environment snapshots и human-only gates для SSH/cloud/cluster.

Критерий готовности Safe Auto: набор потенциально опасных сценариев не достигает
`Execution`, а reviewer outage не увеличивает число разрешённых действий.

## Первичные источники

- [Claude Code permission modes](https://code.claude.com/docs/en/permission-modes)
- [Claude Code Auto mode configuration](https://code.claude.com/docs/en/auto-mode-config)
- [Anthropic engineering deep dive, 25 марта 2026](https://www.anthropic.com/engineering/claude-code-auto-mode)
