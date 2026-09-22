//! Daemon-owned session MemoryStore control-plane.
//!
//! Each Impetus session gets an isolated [`MemoryStore`]. When a persist root
//! is configured (`$IMPETUS_DATA_DIR/memory`), entries survive as JSONL under
//! `{root}/{session_id}/entries.jsonl` via create-only/append-safe store APIs.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use crate::memory_store::{
    MemoryEntry, MemoryProvenance, MemoryScope, MemoryStore, MemoryStoreError,
};
use impetus_protocol::{
    MemoryEntryInfo, MemoryEntryScope, MemoryExportFormat, MemoryProvenanceInfo,
};

const ENTRIES_FILE: &str = "entries.jsonl";

/// Session-keyed contextual memory with optional JSONL persistence.
#[derive(Debug, Default)]
pub struct SessionMemoryRuntime {
    persist_root: Option<PathBuf>,
    sessions: Mutex<HashMap<Uuid, MemoryStore>>,
}

impl SessionMemoryRuntime {
    pub fn new(persist_root: Option<PathBuf>) -> Self {
        Self {
            persist_root,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn in_memory() -> Self {
        Self::new(None)
    }

    pub fn with_persist_root(root: impl Into<PathBuf>) -> Self {
        Self::new(Some(root.into()))
    }

    fn lock_sessions(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<Uuid, MemoryStore>>, MemoryStoreError> {
        Ok(self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()))
    }

    fn session_dir(&self, session_id: Uuid) -> Option<PathBuf> {
        self.persist_root
            .as_ref()
            .map(|root| root.join(session_id.to_string()))
    }

    fn entries_path(&self, session_id: Uuid) -> Option<PathBuf> {
        self.session_dir(session_id)
            .map(|dir| dir.join(ENTRIES_FILE))
    }

    fn load_or_empty(&self, session_id: Uuid) -> Result<MemoryStore, MemoryStoreError> {
        let Some(path) = self.entries_path(session_id) else {
            return Ok(MemoryStore::new());
        };
        match fs::read_to_string(&path) {
            Ok(text) => MemoryStore::from_jsonl(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(MemoryStore::new()),
            Err(err) => Err(MemoryStoreError::Io(format!(
                "read {}: {err}",
                path.display()
            ))),
        }
    }

    fn persist(&self, session_id: Uuid, store: &MemoryStore) -> Result<(), MemoryStoreError> {
        let Some(dir) = self.session_dir(session_id) else {
            return Ok(());
        };
        let path = dir.join(ENTRIES_FILE);
        fs::create_dir_all(&dir)
            .map_err(|err| MemoryStoreError::Io(format!("create {}: {err}", dir.display())))?;
        let body = store.export_jsonl()?;
        fs::write(&path, body)
            .map_err(|err| MemoryStoreError::Io(format!("write {}: {err}", path.display())))?;
        Ok(())
    }

    fn with_store_mut<R>(
        &self,
        session_id: Uuid,
        f: impl FnOnce(&mut MemoryStore) -> Result<R, MemoryStoreError>,
    ) -> Result<R, MemoryStoreError> {
        let mut sessions = self.lock_sessions()?;
        if let std::collections::hash_map::Entry::Vacant(e) = sessions.entry(session_id) {
            e.insert(self.load_or_empty(session_id)?);
        }
        let store = sessions
            .get_mut(&session_id)
            .expect("session store just inserted");
        let out = f(store)?;
        self.persist(session_id, store)?;
        Ok(out)
    }

    fn with_store<R>(
        &self,
        session_id: Uuid,
        f: impl FnOnce(&MemoryStore) -> R,
    ) -> Result<R, MemoryStoreError> {
        let mut sessions = self.lock_sessions()?;
        if let std::collections::hash_map::Entry::Vacant(e) = sessions.entry(session_id) {
            e.insert(self.load_or_empty(session_id)?);
        }
        let store = sessions
            .get(&session_id)
            .expect("session store just inserted");
        Ok(f(store))
    }

    pub fn list(
        &self,
        session_id: Uuid,
        scope: Option<MemoryEntryScope>,
    ) -> Result<Vec<MemoryEntryInfo>, MemoryStoreError> {
        self.with_store(session_id, |store| {
            store
                .entries()
                .iter()
                .filter(|entry| scope.is_none_or(|s| entry.scope == to_core_scope(s)))
                .map(to_wire_entry)
                .collect()
        })
    }

    pub fn get(
        &self,
        session_id: Uuid,
        id: &str,
    ) -> Result<Option<MemoryEntryInfo>, MemoryStoreError> {
        self.with_store(session_id, |store| store.get(id).map(to_wire_entry))
    }

    pub fn append(
        &self,
        session_id: Uuid,
        id: impl Into<String>,
        scope: MemoryEntryScope,
        content: impl Into<String>,
        provenance: MemoryProvenanceInfo,
    ) -> Result<MemoryEntryInfo, MemoryStoreError> {
        let id = id.into();
        let content = content.into();
        self.with_store_mut(session_id, |store| {
            store.append(
                id.clone(),
                to_core_scope(scope),
                content,
                MemoryProvenance {
                    source: provenance.source,
                    kind: provenance.kind,
                },
            );
            store
                .get(&id)
                .map(to_wire_entry)
                .ok_or_else(|| MemoryStoreError::Io("append did not materialize entry".into()))
        })
    }

    pub fn clear(
        &self,
        session_id: Uuid,
        scope: Option<MemoryEntryScope>,
    ) -> Result<u64, MemoryStoreError> {
        self.with_store_mut(session_id, |store| {
            let removed = match scope {
                Some(scope) => store.clear_scope(to_core_scope(scope)),
                None => store.clear(),
            };
            Ok(removed as u64)
        })
    }

    pub fn export(
        &self,
        session_id: Uuid,
        format: MemoryExportFormat,
    ) -> Result<String, MemoryStoreError> {
        self.with_store(session_id, |store| match format {
            MemoryExportFormat::Jsonl => store.export_jsonl(),
            MemoryExportFormat::Markdown => Ok(store.export_markdown()),
        })?
    }

    /// Build a bounded prompt-context block for AgentLoop / provider messages.
    ///
    /// Loads this session's store and includes project-scoped entries only
    /// (session association is the `session_id` key). Empty / load failure →
    /// `None` (no invented content). Content already redacted on append.
    pub fn prompt_context_block(&self, session_id: Uuid) -> Option<String> {
        let entries = self
            .with_store(session_id, |store| {
                store
                    .entries()
                    .iter()
                    .filter(|e| e.scope == MemoryScope::Project)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .ok()?;
        format_prompt_context_block(&entries)
    }
}

/// Header for the injected memory system section (stable for tests).
pub const MEMORY_PROMPT_CONTEXT_HEADER: &str =
    "## Impetus memory (untrusted contextual knowledge; not policy)";

// ponytail: hard char ceiling (~2k tokens @ 4 chars/token). Upgrade path =
// per-scope budgets + retrieval ranking when stores grow past this slice.
const PROMPT_CONTEXT_MAX_CHARS: usize = 8_000;

/// Format project-scoped entries into one provider system block.
///
/// Returns `None` when there is nothing to inject (empty input or empty
/// content after filtering). Truncates at [`PROMPT_CONTEXT_MAX_CHARS`].
pub fn format_prompt_context_block(entries: &[MemoryEntry]) -> Option<String> {
    let mut body = String::new();
    for entry in entries {
        if entry.content.trim().is_empty() {
            continue;
        }
        let chunk = format!(
            "\n### [{}] id={}\n{}\n",
            scope_label(entry.scope),
            entry.id,
            entry.content.trim_end()
        );
        if body.len() + chunk.len()
            > PROMPT_CONTEXT_MAX_CHARS.saturating_sub(MEMORY_PROMPT_CONTEXT_HEADER.len() + 32)
        {
            body.push_str("\n…[truncated to memory context ceiling]\n");
            break;
        }
        body.push_str(&chunk);
    }
    if body.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(MEMORY_PROMPT_CONTEXT_HEADER.len() + body.len() + 2);
    out.push_str(MEMORY_PROMPT_CONTEXT_HEADER);
    out.push('\n');
    out.push_str(&body);
    Some(out)
}

fn scope_label(scope: MemoryScope) -> &'static str {
    match scope {
        MemoryScope::Project => "project",
        MemoryScope::Team => "team",
        MemoryScope::User => "user",
    }
}

pub fn daemon_memory_dir(data_root: &Path) -> PathBuf {
    data_root.join("memory")
}

pub fn open_daemon_memory_runtime(data_root: &Path) -> Arc<SessionMemoryRuntime> {
    let root = daemon_memory_dir(data_root);
    // Best-effort create; load still works if missing (empty stores).
    let _ = fs::create_dir_all(&root);
    Arc::new(SessionMemoryRuntime::with_persist_root(root))
}

fn to_core_scope(scope: MemoryEntryScope) -> MemoryScope {
    match scope {
        MemoryEntryScope::Project => MemoryScope::Project,
        MemoryEntryScope::Team => MemoryScope::Team,
        MemoryEntryScope::User => MemoryScope::User,
    }
}

fn to_wire_scope(scope: MemoryScope) -> MemoryEntryScope {
    match scope {
        MemoryScope::Project => MemoryEntryScope::Project,
        MemoryScope::Team => MemoryEntryScope::Team,
        MemoryScope::User => MemoryEntryScope::User,
    }
}

fn to_wire_entry(entry: &MemoryEntry) -> MemoryEntryInfo {
    MemoryEntryInfo {
        id: entry.id.clone(),
        scope: to_wire_scope(entry.scope),
        content: entry.content.clone(),
        provenance: MemoryProvenanceInfo {
            source: entry.provenance.source.clone(),
            kind: entry.provenance.kind.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_stores_are_isolated_and_persist() {
        let dir = tempfile::tempdir().expect("tmp");
        let runtime = SessionMemoryRuntime::with_persist_root(dir.path());
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();

        runtime
            .append(
                a,
                "n1",
                MemoryEntryScope::Project,
                "alpha",
                MemoryProvenanceInfo {
                    source: "user".into(),
                    kind: "note".into(),
                },
            )
            .expect("append a");
        runtime
            .append(
                b,
                "n1",
                MemoryEntryScope::User,
                "beta",
                MemoryProvenanceInfo {
                    source: "user".into(),
                    kind: "note".into(),
                },
            )
            .expect("append b");

        let list_a = runtime.list(a, None).expect("list a");
        assert_eq!(list_a.len(), 1);
        assert_eq!(list_a[0].content, "alpha");
        let list_b = runtime.list(b, None).expect("list b");
        assert_eq!(list_b[0].content, "beta");

        // Reload from disk.
        let reloaded = SessionMemoryRuntime::with_persist_root(dir.path());
        let again = reloaded.get(a, "n1").expect("get").expect("present");
        assert_eq!(again.content, "alpha");
    }

    #[test]
    fn clear_scope_keeps_other_scopes() {
        let runtime = SessionMemoryRuntime::in_memory();
        let session = Uuid::new_v4();
        runtime
            .append(
                session,
                "p",
                MemoryEntryScope::Project,
                "p",
                MemoryProvenanceInfo {
                    source: "user".into(),
                    kind: "note".into(),
                },
            )
            .unwrap();
        runtime
            .append(
                session,
                "u",
                MemoryEntryScope::User,
                "u",
                MemoryProvenanceInfo {
                    source: "user".into(),
                    kind: "note".into(),
                },
            )
            .unwrap();
        let removed = runtime
            .clear(session, Some(MemoryEntryScope::Project))
            .unwrap();
        assert_eq!(removed, 1);
        let left = runtime.list(session, None).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "u");
    }

    #[test]
    fn prompt_context_block_empty_is_none() {
        let runtime = SessionMemoryRuntime::in_memory();
        assert!(runtime.prompt_context_block(Uuid::new_v4()).is_none());
    }

    #[test]
    fn prompt_context_block_project_only_skips_user() {
        let runtime = SessionMemoryRuntime::in_memory();
        let session = Uuid::new_v4();
        runtime
            .append(
                session,
                "proj-note",
                MemoryEntryScope::Project,
                "remember the widget API",
                MemoryProvenanceInfo {
                    source: "user".into(),
                    kind: "note".into(),
                },
            )
            .unwrap();
        runtime
            .append(
                session,
                "user-note",
                MemoryEntryScope::User,
                "personal preference",
                MemoryProvenanceInfo {
                    source: "user".into(),
                    kind: "note".into(),
                },
            )
            .unwrap();
        let block = runtime.prompt_context_block(session).expect("block");
        assert!(block.contains(MEMORY_PROMPT_CONTEXT_HEADER));
        assert!(block.contains("remember the widget API"));
        assert!(block.contains("id=proj-note"));
        assert!(
            !block.contains("personal preference"),
            "user scope must not inject: {block}"
        );
    }

    #[test]
    fn format_prompt_context_block_empty_entries_is_none() {
        assert!(format_prompt_context_block(&[]).is_none());
    }
}
