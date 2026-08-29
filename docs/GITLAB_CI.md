# GitLab CI в Impetus

## Граница v0.2

Панель читает `.gitlab-ci.yml` и отображает уже существующий pipeline. Она не создаёт отдельный формат, не запускает GitLab server и не подменяет runner.

```text
gitlab-ci-local --list-csv-all / local stdout+stderr  → LocalGitlabBackend
glab ci list --output json + glab api                 → RemoteGitlabBackend
                                                     ↓
                                      Pipeline → Stage → Job → native renderer
```

Локальная ветка доступна только после явного **Run local**. Удалённая — после **Remote status** и только когда `origin` указывает на GitLab. В обоих случаях stdout tool-а захватывается приложением, а не смешивается с UI stdout.

В этом репозитории source of truth — `.gitlab-ci.yml`: stage `verify` содержит независимые jobs `fmt`, `test`, `check`, `clippy`, затем stage `security` запускает `cargo-audit` и `cargo-deny`. CI image и security tools закреплены: `rust:1.98.0-bookworm`, `cargo-audit 0.22.2`, `cargo-deny 0.20.2`. Изменение обязательной Rust-проверки или dependency policy требует синхронно обновить pipeline.

Для trusted local smoke используются `task ci:list` и `task ci:local`. Последняя команда передаёт `.gitlab-ci.yml` в `gitlab-ci-local --force-shell-executor --shell-executor-no-image`: проверяются jobs и их скрипты с локальным Rust, но container image не подменяется в самом GitLab pipeline.

## Доступное сейчас

- local: обнаружение `.gitlab-ci.yml`, список jobs/stages через `--list-csv-all`, запуск `gitlab-ci-local`, статус pipeline, duration, success/failure и compact error fragment;
- remote: status текущей ветки и jobs последнего pipeline через структурированный JSON `glab`;
- disclosure: `↑`/`↓`, `Enter`, `l` и `q`; remote job trace загружается только при открытии лога;
- error extraction: Cargo/Rust, npm/Node, shell exit и generic stderr по `error:`, `fatal:`, `panic`, `FAILED`, `AssertionError`, `error[E…]`.

## Local output

Во время **Run local** панель показывает последние 12 строк как компактный terminal-like preview: command/running — cyan, success — green, warning — yellow, failure — red. Это presentation поверх очищенных строк `gitlab-ci-local`, не ANSI/PTY renderer и не второй log format. Полный сохранённый visible buffer открывается через `l`; после 600 строк UI явно сообщает о dropped старом хвосте.

## Явно не входит

`ci run`, `retry` и `cancel` для GitLab remote, граф pipeline, dashboard и AI-summary. Remote mutation появится только после approval UX с точным pipeline/job target. Видимый local live buffer ограничен 600 строками, но producer channel и полный log пока не bounded; общий v0.2 artifact store должен заменить этот временный client buffer.

## Требования для smoke

- `.gitlab-ci.yml` в workspace;
- установленный `gitlab-ci-local` для local run;
- установленный и уже авторизованный `glab` для remote status.
