# Impetus: последовательное продолжение разработки

Цель: пройти оставшиеся архитектурные слабые места Impetus **по одному независимому slice за раз**.

Исполняй файлы в этом каталоге по порядку номеров. Каждый prompt — отдельный GitHub Issue/branch/PR и должен быть merged до начала следующего, кроме случая, когда соответствующая работа уже полностью существует в свежем `main`.

Приоритет:
1. убрать устаревшую концепцию из роутинга и документации;
2. безопасность исполнения;
3. контекст и сессии;
4. ACP;
5. реальные runtime-интеграции;
6. web/TUI/reference/auth;
7. архитектурная уборка, миграции и документационная правда.

Не создавай пачку веток одновременно. После каждого merge заново подтягивай `main`, потому что следующий prompt должен оцениваться относительно уже изменённого дерева.

Файлы:
- `01_REMOVE_LOCAL_CLOUD_RESEARCH_ESCALATION.md`
- `02_PRODUCTION_MACOS_SANDBOX.md`
- `03_CONTEXT_OPTIMIZER.md`
- `04_SESSION_DAG_SHARED_PREFIX.md`
- `05_ACP_PRODUCTION_HARDENING.md`
- `06_MODEL_ROUTER_REAL_INTEGRATION.md`
- `07_MODULE_RUNTIME_REAL_LIFECYCLE.md`
- `08_PROVIDER_NATIVE_TOOL_CALLS_AND_USAGE.md`
- `09_WEB_RESEARCH_COMPLETION.md`
- `10_TUI_EXECUTION_MODES_AND_LARGE_PASTE.md`
- `11_REFERENCE_STORE_GENERALIZATION.md`
- `12_DIRECT_PROVIDER_AUTH_HARDENING.md`
- `13_CLIENT_BOUNDARIES_AND_CORE_DECOMPOSITION.md`
- `14_SECURITY_E2E_MIGRATIONS_AND_DOC_TRUTH.md`

Для каждого файла действует workflow, описанный внутри него.
