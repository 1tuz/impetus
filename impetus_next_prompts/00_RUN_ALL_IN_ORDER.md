# Impetus: sequential development continuation

Goal: address remaining Impetus architectural weak spots **one independent slice at a time**.

Execute files in this directory in numerical order. Each prompt is a separate GitHub Issue/branch/PR and must be merged before starting the next, except when the corresponding work already fully exists in fresh `main`.

Priority:
1. remove outdated concept from routing and documentation;
2. execution security;
3. context and sessions;
4. ACP;
5. real runtime integrations;
6. web/TUI/reference/auth;
7. architectural cleanup, migrations, and documentation truth.

Do not create a batch of branches simultaneously. After each merge, pull `main` again, because the next prompt must be evaluated against the already changed tree.

Files:
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

For each file, the workflow described inside it applies.
