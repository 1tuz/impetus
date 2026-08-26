# GUI/UX: терминал, агент и Blocks в одном окне

## Главный принцип

Агент не заменяет терминал и не прячется в отдельном «чате». У пользователя одна рабочая поверхность: активный terminal tab, natural-language composer и общая лента Blocks. Любой рискованный шаг читается и подтверждается до исполнения.

Текущий v0.1 UI — диагностический native preview: окно, палитры terminal themes, session ID и состояние durable runtime. Terminal input, адаптивный split и Blocks относятся к v0.2–v0.3; наличие их визуального placeholder не означает готовую функцию.

## Базовый макет v0.2–v0.3

```text
┌ Window toolbar ─────────────────────────────────────────────────────────────┐
│ ◉ Local workspace  ▾    [+ Tab]   Sessions   Search   Safe Auto ▾  ⚙      │
├ Tabs ──────────────────────────────────────────────────────────────────────┤
│ ● zsh: api  ×     ○ deploy-check  ×     +                                  │
├──────────── Terminal ─────────────┬────────── Blocks / Agent ──────────────┤
│ $ kubectl get pods                │ Plan                                  │
│ NAME ...                           │ 1. inspect logs       read-only      │
│                                    │ 2. compare config     read-only      │
│                                    │                                      │
│                                    │ Approval required                     │
│                                    │ restart nginx                         │
│                                    │ [Inspect] [Approve once] [Reject]    │
├────────────────────────────────────┴───────────────────────────────────────┤
│ [+ File] [Capture] Ask the agent or type /command…           ⌘↵ Send       │
└────────────────────────────────────────────────────────────────────────────┘
```

Breakpoints задаются поведением, а не названием устройства:

- `≥ 1100 px`: terminal и Blocks видны одновременно; terminal получает 60–75% ширины;
- `760–1099 px`: Blocks — overlay справа шириной `min(420 px, 40%)`, terminal под ним не relayout-ится;
- `< 760 px`: одновременно видна одна поверхность, переключение `Terminal | Blocks` находится в toolbar; composer остаётся закреплён снизу.

Composer всегда один, но его destination всегда виден отдельным label: `Agent`, `Command palette` или `Terminal paste`. Префикс `/` открывает command palette, обычный текст создаёт agent intent. Многострочная shell-вставка никогда не исполняется по обычному Send: UI показывает preview, target tab и отдельное `Send to terminal`. Переключение destination не сохраняет скрытый черновик другого режима.

Mode chip `Manual | Safe Auto | Auto paused` постоянно виден в toolbar. Переключение — только прямое user action; agent output не может вызвать его программно. `Safe Auto` не скрывает блокировки: Block показывает action/target, стабильную категорию причины и безопасную альтернативу. У classifier denial нет кнопки «run anyway»; повтор возможен после явного перехода в Manual и новой проверки. Подробный контракт: [SAFE_AUTO_MODE.md](SAFE_AUTO_MODE.md).

File, drag-and-drop, paste image и Capture сначала создают attachment chips с preview, типом, размером, scan state и exact backend. Send недоступен до проверки и capability negotiation; удалить chip можно без удаления исходного файла. Фоновый screen capture и автоматическое добавление найденного агентом файла запрещены. Подробный поток: [ATTACHMENTS.md](ATTACHMENTS.md).

## Темы терминала

Тема относится к terminal UI, не к policy/runtime, и меняет только отображение: background, foreground, cursor, selection и все 16 ANSI-цветов. Встроенный набор доступен без сети и сторонних расширений: Dracula (default), One Dark, Nord, Tokyo Night, Gruvbox Dark, Catppuccin Mocha, Solarized Light, One Light, GitHub Light и Catppuccin Latte. Переключение немедленно обновляет preview; сохранение выбора между запусками появится вместе с пользовательскими настройками, а PTY renderer v0.2 использует те же палитры.

## Информационная иерархия

1. **Где я работаю:** toolbar постоянно показывает Local / имя SSH profile, рабочий каталог и состояние подключения.
2. **Что сейчас происходит:** активный terminal tab и последний running Block различимы без чтения ленты.
3. **Что требует человека:** Approval Block закрепляется над лентой и показывает точный effect: diff, command, host или file transfer.
4. **Что произошло:** normal tool output остаётся в timeline, но краткий status не конкурирует с терминалом.

Не использовать модальные окна для обычного agent output. Modal допустим для первого SSH fingerprint или необратимого массового действия: там решение нельзя принять, не увидев identity/scope.

## Blocks — переиспользуемая модель вместо отдельных экранов

Никакой особый UI-код не должен быть «только для агента». Один `Block` с discriminated kind и view model обслуживает ленту, поиск, export, resume/fork и уведомления.

| Kind | Содержимое | Действия |
| --- | --- | --- |
| Intent | фраза пользователя + workspace | edit/retry/fork |
| Plan | шаги, scope, status | раскрыть evidence/cancel |
| Terminal | привязка к tab + диапазон scrollback | открыть/скопировать |
| Tool | capability, target, итог | раскрыть redacted output |
| Approval | точный diff/command/host/file и reason | inspect/approve once/reject |
| Notice | completion, disconnect, recovery | открыть сессию |
| Summary | compaction source range и версия | показать исходные Blocks |

Это повторно использует существующие `Event`, `ApprovalRequest`, `PolicyDecision` и capability manifest из `agentic-terminal-core`. UI не хранит параллельный state machine: projection строится из event stream.

## Состояния, которые должны быть явными

| Поверхность | Обязательные состояния |
| --- | --- |
| Terminal tab | starting, running, exited(code), failed, reconnecting, disconnected |
| Agent run | planning, streaming, waiting approval, cancelling, cancelled, completed, failed |
| Approval | pending, inspecting, approved, rejected, expired, invalidated by changed target |
| Safe Auto | manual, active, checking, blocked, paused, reviewer unavailable |
| Attachment | inspecting, ready, unsupported, sensitive, oversize, uploading, sent, failed, removed |
| Backend profile | connected, needs login, unavailable, incompatible, crashed |
| Block output | empty, partial, complete, redacted, truncated с ссылкой на durable source range |

Ни spinner, ни цвет не являются единственным сигналом. После crash/restart UI строит состояние из событий; если terminal/capability outcome неизвестен, показывает `Interrupted — outcome unknown`, а не `Completed`.

## Focus и ввод

Одновременно существует один focus owner: terminal, composer, Blocks list, approval actions или overlay. Правила приоритета:

1. `Ctrl-C` уходит только в сфокусированный PTY; он не отменяет agent run.
2. `Esc` сначала закрывает transient overlay, затем отменяет agent run только при focus в composer/Blocks; terminal получает свой Escape byte.
3. Approval не перехватывает Enter. Кнопки доступны Tab/Shift-Tab, а подтверждение требует Space либо явного click на сфокусированной кнопке.
4. После закрытия overlay focus возвращается элементу, который его открыл. После закрытия tab — ближайшему оставшемуся tab, затем composer.
5. Agent не может программно перевести focus в approval-кнопку или PTY.

## Что переиспользовать, а не писать с нуля

| Поверхность | Основа | Почему |
| --- | --- | --- |
| окно, layout, focus, keybindings | GPUI-CE и его официальные examples | нативный Metal-путь без браузерного рантайма |
| shell process / resize | `portable-pty` | зрелая граница PTY вместо `Command::output` |
| ANSI/VT parser | оценить `alacritty_terminal` в v0.2 | терминальная совместимость и scrollback semantics уже решены |
| local secrets | Keychain adapter (`security-framework` или узкая system wrapper) | не изобретать собственный secret storage |
| SSH transport | `russh` после отдельного spike | async Rust, но profile/known-host остаются нашими policy boundaries |
| capability composition | JSON manifests + typed Rust traits | идея DSH «capability собираются из конфигурации», без Node/Cordis runtime внутри app |

## Capability construction, вдохновлённая DeepSeek Harness

`config/capabilities.json` — это **каталог**, а не список произвольных shell-команд. На старте runtime:

1. читает manifest;
2. валидирует `id`, semver, permissions и совместимость core API;
3. создаёт implementation через статический Rust registry;
4. выдаёт capability только ограниченный `CapabilityContext` (event writer, approved scope, cancellation token);
5. отказывает capability, если manifest просит несуществующее/неодобренное право.

Во v0.5 внешние плагины сначала запускаются out-of-process по versioned IPC. Dynamic loading в UI process не входит в MVP: это ломает crash/permission boundary и не нужно для ценности конструктора.

## Горячие клавиши и accessibility

- `⌘↵` — отправить intent; `Esc` — отменить текущий agent run (не terminal process).
- `⌘⇧↵` — отправить выделенный terminal output как контекст с явным preview.
- `⌘⇧A` — открыть native file picker; screen capture получает отдельную команду и системный permission.
- `⌘K` — command palette; `⌘1…9` — terminal tabs; `⌘.` — focus composer.
- `Enter` в Approval — ничего не одобряет. Approval требует отдельной кнопки и focus-visible confirmation.
- Каждое состояние цвета имеет текст: `Pending approval`, `Blocked by policy`, `Completed`, `Cancelled`.

Каждый Block и tab имеет accessibility role, короткое label и state/position (`Plan, running, 2 of 8`). Порядок VoiceOver совпадает с визуальным: toolbar → tabs → terminal → Blocks → composer; закреплённый approval объявляется как новый region, но не крадёт focus. Минимальная hit-area — 32×32 pt, focus ring не заменяется цветом темы, а terminal palette обязана сохранять контраст UI chrome независимо от ANSI цветов. `prefers-reduced-motion` отключает необязательные transitions; streaming остаётся читаемым без анимации.

## Темы и визуальные токены

Terminal theme управляет только terminal surface, cursor, selection и ANSI palette. Toolbar, approval, focus ring, ошибки и policy states используют отдельные application semantic tokens; ANSI red нельзя трактовать как `Denied`. Для каждой встроенной light/dark темы нужны contrast smoke и screenshot regression. Выбор темы хранится как её стабильный ID, неизвестный ID безопасно откатывается к системной/default теме.

## Проверяемость GUI

Для каждого нового Block добавить: pure projection test в core, GPUI entity/view test на точный state, keyboard/focus test, VoiceOver label assertion и screenshot regression на macOS в light/dark теме и на одном узком breakpoint. Сначала тестируется event→projection, затем отрисовка; так UI не становится единственным местом бизнес-логики.
