# macOS sandbox spike

Этот spike проверяет доступный системный механизм до включения agent-initiated
write/process/network effects. Это не production executor и не разрешение
включить такие effects.

## Подтверждённая граница

На macOS используется системный `/usr/bin/sandbox-exec` (Seatbelt) с
fail-closed profile:

```scheme
(version 1)
(deny default)
(allow process-exec)
(allow file-read*)
(allow file-write* (subpath "<canonical-workspace>"))
```

В proof один sandboxed `/usr/bin/touch` создаёт файл только в canonical
workspace. Попытка записать sibling path завершается non-zero и не создаёт
файл. Profile не выдаёт network permission, не передаёт secrets и не запускает
agent command.

Воспроизведение на macOS:

```zsh
./scripts/smoke-macos-sandbox.sh
cargo test -p impetus-core --test macos_sandbox_spike
```

Rust test выполняется только на macOS; на другой платформе он пустой. На
macOS отсутствие `/usr/bin/sandbox-exec` — failure, а не fallback to an
unrestricted child process.

## Что это не закрывает

- `ReadOnlySandbox` остаётся логической gate для уже read-only capability и
  пока не объявляется OS sandbox implementation.
- Нет agent-initiated write, process или network capability, и нет API для
  произвольного Seatbelt profile.
- Нет разрешения network, PTY, SSH, tmux, SFTP или external dependencies.

Перед включением любого mutating capability нужен отдельный design и test:
canonical target должен быть включён в generated profile harness-ом, policy и
exact approval/revision должны проверяться до spawn, а unavailable Seatbelt
или profile generation обязан давать durable fail-closed outcome. Capability
не может принимать profile text от client, provider или model.
