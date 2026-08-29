//! Durable artifact store with SHA-256 content addressing.
//!
//! Artifacts are large tool outputs stored separately from the event log,
//! referenced by content hash. Metadata survives daemon restarts.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Durable artifact store with SQLite index
pub struct DurableArtifactStore {
    root: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub id: String,
    pub byte_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactMeta {
    pub id: String,
    pub byte_count: usize,
    pub created_unix_ms: u64,
    pub sha256: String,
}

impl DurableArtifactStore {
    /// Open or create artifact store at the given root
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;

        let db_path = root.join("artifacts.db");
        let conn = Connection::open(db_path)?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS artifacts (
                id TEXT PRIMARY KEY,
                sha256 TEXT NOT NULL UNIQUE,
                byte_count INTEGER NOT NULL,
                created_unix_ms INTEGER NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_artifacts_sha256 ON artifacts(sha256)",
            [],
        )?;

        Ok(Self {
            root,
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Store bytes and return a content-addressed reference
    pub fn store(&self, bytes: &[u8]) -> Result<ArtifactRef> {
        let sha256 = compute_sha256(bytes);
        let id = sha256.clone(); // Use SHA-256 as ID
        let byte_count = bytes.len();
        let created_unix_ms = now_unix_ms();

        let conn = self.conn.lock().unwrap();

        // Check if already exists
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM artifacts WHERE sha256 = ?1",
                params![&sha256],
                |_| Ok(true),
            )
            .unwrap_or(false);

        if !exists {
            // Write content file
            let path = self.root.join(&id);
            std::fs::write(&path, bytes)?;

            // Insert metadata
            conn.execute(
                "INSERT INTO artifacts (id, sha256, byte_count, created_unix_ms) VALUES (?1, ?2, ?3, ?4)",
                params![&id, &sha256, byte_count as i64, created_unix_ms as i64],
            )?;
        }

        Ok(ArtifactRef { id, byte_count })
    }

    /// Read a byte range from an artifact
    pub fn read_range(&self, id: &str, start: usize, len: usize) -> Result<Vec<u8>> {
        let path = self.root.join(id);
        let bytes = std::fs::read(&path)?;
        let end = (start + len).min(bytes.len());
        Ok(bytes.get(start..end).unwrap_or(&[]).to_vec())
    }

    /// Read entire artifact
    pub fn read(&self, id: &str) -> Result<Vec<u8>> {
        let path = self.root.join(id);
        Ok(std::fs::read(&path)?)
    }

    /// Get artifact metadata
    pub fn metadata(&self, id: &str) -> Result<Option<ArtifactMeta>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT id, sha256, byte_count, created_unix_ms FROM artifacts WHERE id = ?1",
            params![id],
            |row| {
                Ok(ArtifactMeta {
                    id: row.get(0)?,
                    sha256: row.get(1)?,
                    byte_count: row.get::<_, i64>(2)? as usize,
                    created_unix_ms: row.get::<_, i64>(3)? as u64,
                })
            },
        );

        match result {
            Ok(meta) => Ok(Some(meta)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// List all artifacts
    pub fn list(&self) -> Result<Vec<ArtifactMeta>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, sha256, byte_count, created_unix_ms FROM artifacts ORDER BY created_unix_ms DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(ArtifactMeta {
                id: row.get(0)?,
                sha256: row.get(1)?,
                byte_count: row.get::<_, i64>(2)? as usize,
                created_unix_ms: row.get::<_, i64>(3)? as u64,
            })
        })?;

        let mut artifacts = Vec::new();
        for row in rows {
            artifacts.push(row?);
        }

        Ok(artifacts)
    }

    /// Delete an artifact
    pub fn delete(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM artifacts WHERE id = ?1", params![id])?;

        let path = self.root.join(id);
        if path.exists() {
            std::fs::remove_file(path)?;
        }

        Ok(())
    }

    /// Garbage collect: remove artifacts older than threshold
    pub fn gc_older_than(&self, threshold_unix_ms: u64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id FROM artifacts WHERE created_unix_ms < ?1")?;
        let ids: Vec<String> = stmt
            .query_map(params![threshold_unix_ms as i64], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;

        let count = ids.len();
        for id in ids {
            drop(conn.execute("DELETE FROM artifacts WHERE id = ?1", params![&id]));
            let path = self.root.join(&id);
            drop(std::fs::remove_file(path));
        }

        Ok(count)
    }
}

fn compute_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, DurableArtifactStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DurableArtifactStore::open(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn store_and_retrieve() {
        let (_dir, store) = temp_store();
        let content = b"Hello, world!";
        let art_ref = store.store(content).unwrap();

        assert_eq!(art_ref.byte_count, content.len());

        let retrieved = store.read(&art_ref.id).unwrap();
        assert_eq!(retrieved, content);
    }

    #[test]
    fn content_addressed_deduplication() {
        let (_dir, store) = temp_store();
        let content = b"duplicate content";

        let ref1 = store.store(content).unwrap();
        let ref2 = store.store(content).unwrap();

        assert_eq!(ref1.id, ref2.id, "identical content has same ID");
    }

    #[test]
    fn metadata_survives() {
        let (_dir, store) = temp_store();
        let content = b"persistent metadata";
        let art_ref = store.store(content).unwrap();

        let meta = store.metadata(&art_ref.id).unwrap().unwrap();
        assert_eq!(meta.byte_count, content.len());
        assert_eq!(meta.sha256, art_ref.id);
    }

    #[test]
    fn range_read() {
        let (_dir, store) = temp_store();
        let content = b"the quick brown fox jumps";
        let art_ref = store.store(content).unwrap();

        let slice = store.read_range(&art_ref.id, 4, 5).unwrap();
        assert_eq!(slice, b"quick");
    }

    #[test]
    fn list_artifacts() {
        let (_dir, store) = temp_store();
        store.store(b"first").unwrap();
        store.store(b"second").unwrap();

        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn delete_artifact() {
        let (_dir, store) = temp_store();
        let art_ref = store.store(b"to be deleted").unwrap();

        store.delete(&art_ref.id).unwrap();
        assert!(store.metadata(&art_ref.id).unwrap().is_none());
    }

    #[test]
    fn garbage_collection() {
        let (_dir, store) = temp_store();
        store.store(b"old artifact").unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let threshold = now_unix_ms();
        std::thread::sleep(std::time::Duration::from_millis(10));

        store.store(b"new artifact").unwrap();

        let removed = store.gc_older_than(threshold).unwrap();
        assert_eq!(removed, 1);

        let remaining = store.list().unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn sha256_content_hash() {
        let hash = compute_sha256(b"test");
        assert_eq!(hash.len(), 64); // SHA-256 is 64 hex chars
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
