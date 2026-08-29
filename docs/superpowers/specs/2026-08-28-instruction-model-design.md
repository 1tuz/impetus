# Instruction Model Design

## Goal

Add a small, harness-owned instruction layer that distinguishes identity,
project rules, declarative conventions, domain guides, and procedural skills.
It resolves only the files relevant to a workspace task, without changing the
existing Policy, Sandbox, Capability, Approval, or secret boundaries.

## Current state

The runtime has no instruction discovery or resolver. `IpcRequest::Prompt`
records user text and `OpenAiCompatibleProvider` sends it as one user message.
`plugins.rs` is a capability registry, not an instruction registry.

## Model

The resolver reads this optional workspace layout:

```text
AGENTS.md                         project rules
.impetus/SOUL.md                  agent identity
.impetus/conventions/*.md         concise declarative rules
.impetus/guides/*.md              domain knowledge
.impetus/skills/<name>/SKILL.md   procedural workflows
```

Existing root `AGENTS.md` remains project rules. Existing `SKILL.md` files
remain skills. Optional front matter on a skill may name guide and convention
IDs; referenced source text stays in its own file.

Each discovered object has a stable ID, kind, scope, relative path, content
hash, and text. Scope is deterministic: global, workspace, path, or ecosystem.
The resolver includes only matching objects and emits the fixed order:

`SOUL -> project rules -> conventions -> guides -> selected skills -> user text`.

## Runtime and safety

The harness resolves context after it persists the original user intent. The
provider receives a transient ordered message list. Instruction bodies are not
copied into SQLite events, logs, exports, or IPC prompt fields.

Instruction metadata is advisory only. It cannot modify `ActionOrigin`,
`PolicyEngine`, approvals, sandbox scopes, capability manifests, credentials,
or execution. A declared `requires: ssh-prod` is never an authorization.

## Cache and telemetry

The resolver keeps a bounded, on-demand cache keyed by relative path and
content hash. A changed file reloads only its own entry. Stable lexical order
and stable serialization preserve prompt-prefix cache eligibility.

It reports estimated tokens separately for project rules, conventions, guides,
and skills. These values are explicitly estimates until a provider tokenizer
is integrated.

## Visibility and learning

A version-negotiated context request and CLI subcommand show the live resolved
references and token totals. This is not a TUI implementation.

Self-Repair starts as a proposal-only lifecycle:

`Observed -> Candidate -> Repeated -> Validated -> Proposed -> Promoted`.

It classifies targets as memory, convention, guide update, or skill
improvement. It never creates or modifies instruction files automatically;
skills require a stricter promotion threshold than conventions.

## Compatibility and delivery

The event schema remains unchanged in this slice. The existing
`stream_user_message` API remains a compatibility wrapper around a multi-message
provider API. IPC additions use a new negotiated capability and are implemented
in both Unix and in-memory clients.

The work is delivered in small independently testable slices: documentation,
pure resolver, transient provider/harness integration plus context inspection,
and proposal-only learning classification.
