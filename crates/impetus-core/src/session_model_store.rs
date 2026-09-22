//! Durable per-session model/reasoning selection.
//!
//! In-memory map alone loses overrides across `impetusd` restart. When a root
//! is configured (`$IMPETUS_DATA_DIR/session_models`), each session stores one
//! JSON file `{session_id}.json`. No silent rewrite of unavailable models —
//! callers re-validate against the live provider registry on load.

use std::fs;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use impetus_protocol::SessionModelSelection;

/// File-backed session model selection store.
#[derive(Debug, Clone)]
pub struct SessionModelStore {
    root: PathBuf,
}

impl SessionModelStore {
    pub fn open(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, session_id: Uuid) -> PathBuf {
        self.root.join(format!("{session_id}.json"))
    }

    pub fn load(&self, session_id: Uuid) -> std::io::Result<Option<SessionModelSelection>> {
        let path = self.path_for(session_id);
        match fs::read_to_string(&path) {
            Ok(text) => {
                let selection: SessionModelSelection = serde_json::from_str(&text)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                Ok(Some(selection))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub fn save(&self, session_id: Uuid, selection: &SessionModelSelection) -> std::io::Result<()> {
        fs::create_dir_all(&self.root)?;
        let path = self.path_for(session_id);
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(selection)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(&tmp, body)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn remove(&self, session_id: Uuid) -> std::io::Result<()> {
        let path = self.path_for(session_id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }
}

/// `$IMPETUS_DATA_DIR/session_models`.
pub fn daemon_session_models_dir(data_root: &Path) -> PathBuf {
    data_root.join("session_models")
}

/// Open durable session-model store under the daemon data root.
pub fn open_daemon_session_model_store(data_root: &Path) -> std::io::Result<SessionModelStore> {
    SessionModelStore::open(daemon_session_models_dir(data_root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_missing() {
        let dir = tempfile::tempdir().expect("temp");
        let store = SessionModelStore::open(dir.path()).expect("open");
        let sid = Uuid::new_v4();
        assert!(store.load(sid).expect("load").is_none());
        let selection = SessionModelSelection {
            provider_id: "mock".into(),
            model_id: "mock-fast".into(),
            reasoning_effort: Some("high".into()),
        };
        store.save(sid, &selection).expect("save");
        let loaded = store.load(sid).expect("load").expect("present");
        assert_eq!(loaded, selection);
        store.remove(sid).expect("remove");
        assert!(store.load(sid).expect("load").is_none());
    }
}
