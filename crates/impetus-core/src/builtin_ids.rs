//! Built-in agent / skill / command id inventory + duplicate hygiene (TODO P1 §10).
//!
//! Keeps the shipped set **small and explicit**. Duplicate ids across registry
//! sources are a hard error. Unused detection is a Planned stub (YAGNI) — no
//! recipe/test cross-ref walk yet.
//!
//! Out of scope: marketplace, third-party installs, vendor feature-count parity.

use std::collections::HashMap;

use serde::Serialize;

/// Kind of built-in Impetus ships (not extension/marketplace content).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinKind {
    Agent,
    Skill,
    Command,
}

impl BuiltinKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Skill => "skill",
            Self::Command => "command",
        }
    }
}

/// One registered built-in id with provenance (`source` is a code label, not a secret).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuiltinIdEntry {
    pub kind: BuiltinKind,
    pub id: String,
    pub source: String,
}

impl BuiltinIdEntry {
    pub fn new(kind: BuiltinKind, id: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
            source: source.into(),
        }
    }
}

/// Duplicate id found across one or more registry sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DuplicateBuiltinId {
    pub id: String,
    pub kind: BuiltinKind,
    pub sources: Vec<String>,
    pub count: usize,
}

/// Audit result for a built-in id list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuiltinIdAudit {
    pub entries: Vec<BuiltinIdEntry>,
    pub duplicates: Vec<DuplicateBuiltinId>,
    /// Always empty today — unused cross-ref is Planned (see [`unused_builtin_ids_stub`]).
    pub unused_stub: Vec<String>,
}

impl BuiltinIdAudit {
    pub fn ok(&self) -> bool {
        self.duplicates.is_empty()
    }
}

/// Minimal real set Impetus ships today.
///
/// - **Agents:** four [`crate::SubagentRole`] labels (same as WorkflowEngine /
///   [`crate::AgentWorkRole`]) — no fifth agent type.
/// - **Skills:** none first-party (import adapters only).
/// - **Commands:** none first-party slash/catalog entries in core (TUI palette
///   lives in `impetus-tui`; not duplicated here).
pub fn shipped_builtin_ids() -> Vec<BuiltinIdEntry> {
    vec![
        BuiltinIdEntry::new(BuiltinKind::Agent, "Explore", "SubagentRole"),
        BuiltinIdEntry::new(BuiltinKind::Agent, "Research", "SubagentRole"),
        BuiltinIdEntry::new(BuiltinKind::Agent, "Build", "SubagentRole"),
        BuiltinIdEntry::new(BuiltinKind::Agent, "Review", "SubagentRole"),
    ]
}

/// Find duplicate `(kind, id)` pairs. Same id under different kinds is allowed.
pub fn find_duplicate_ids(entries: &[BuiltinIdEntry]) -> Vec<DuplicateBuiltinId> {
    let mut groups: HashMap<(BuiltinKind, &str), Vec<&str>> = HashMap::new();
    for entry in entries {
        groups
            .entry((entry.kind, entry.id.as_str()))
            .or_default()
            .push(entry.source.as_str());
    }

    let mut duplicates: Vec<DuplicateBuiltinId> = groups
        .into_iter()
        .filter(|(_, sources)| sources.len() > 1)
        .map(|((kind, id), sources)| DuplicateBuiltinId {
            id: id.to_string(),
            kind,
            count: sources.len(),
            sources: sources.into_iter().map(str::to_string).collect(),
        })
        .collect();

    duplicates.sort_by(|a, b| {
        a.kind
            .as_str()
            .cmp(b.kind.as_str())
            .then_with(|| a.id.cmp(&b.id))
    });
    duplicates
}

/// Planned unused detection: registered but never referenced in recipes/tests.
///
/// Returns empty always — walking recipes/tests is deferred (YAGNI). Callers
/// should treat this as a stub, not a clean bill of health for unused ids.
pub fn unused_builtin_ids_stub(_entries: &[BuiltinIdEntry]) -> Vec<String> {
    Vec::new()
}

/// Audit an arbitrary entry list (tests / extra sources).
pub fn audit_builtin_ids(entries: Vec<BuiltinIdEntry>) -> BuiltinIdAudit {
    let duplicates = find_duplicate_ids(&entries);
    let unused_stub = unused_builtin_ids_stub(&entries);
    BuiltinIdAudit {
        entries,
        duplicates,
        unused_stub,
    }
}

/// Audit the shipped Impetus inventory.
pub fn audit_shipped_builtin_ids() -> BuiltinIdAudit {
    audit_builtin_ids(shipped_builtin_ids())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_set_is_small_and_unique() {
        let audit = audit_shipped_builtin_ids();
        assert!(audit.ok(), "shipped builtins must have no duplicates");
        assert_eq!(audit.entries.len(), 4);
        assert!(audit.unused_stub.is_empty());
        let agents: Vec<_> = audit
            .entries
            .iter()
            .filter(|e| e.kind == BuiltinKind::Agent)
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(agents, ["Explore", "Research", "Build", "Review"]);
        assert!(
            !audit
                .entries
                .iter()
                .any(|e| e.kind == BuiltinKind::Skill || e.kind == BuiltinKind::Command)
        );
    }

    #[test]
    fn detects_duplicate_agent_ids() {
        let entries = vec![
            BuiltinIdEntry::new(BuiltinKind::Agent, "Build", "SubagentRole"),
            BuiltinIdEntry::new(BuiltinKind::Agent, "Build", "extra_source"),
            BuiltinIdEntry::new(BuiltinKind::Agent, "Explore", "SubagentRole"),
        ];
        let dups = find_duplicate_ids(&entries);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].id, "Build");
        assert_eq!(dups[0].kind, BuiltinKind::Agent);
        assert_eq!(dups[0].count, 2);
        assert!(dups[0].sources.contains(&"SubagentRole".into()));
        assert!(dups[0].sources.contains(&"extra_source".into()));
        assert!(!audit_builtin_ids(entries).ok());
    }

    #[test]
    fn same_id_different_kinds_not_duplicate() {
        let entries = vec![
            BuiltinIdEntry::new(BuiltinKind::Agent, "review", "agents"),
            BuiltinIdEntry::new(BuiltinKind::Command, "review", "commands"),
        ];
        assert!(find_duplicate_ids(&entries).is_empty());
        assert!(audit_builtin_ids(entries).ok());
    }

    #[test]
    fn unused_stub_is_always_empty() {
        let entries = shipped_builtin_ids();
        assert!(unused_builtin_ids_stub(&entries).is_empty());
        // Even with a bogus id, stub does not invent unused reports.
        let mut with_orphan = entries;
        with_orphan.push(BuiltinIdEntry::new(
            BuiltinKind::Skill,
            "orphan-skill",
            "test",
        ));
        assert!(unused_builtin_ids_stub(&with_orphan).is_empty());
    }

    #[test]
    fn audit_serializes_without_secrets() {
        let audit = audit_shipped_builtin_ids();
        let json = serde_json::to_string(&audit).expect("serialize");
        assert!(json.contains("Explore"));
        assert!(!json.contains("sk-"));
        assert!(!json.to_lowercase().contains("password"));
    }
}
