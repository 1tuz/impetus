//! Durable ownership records for managed resources.
//!
//! First-class invariant (TODO P1 §3):
//! `destination exists + no matching Impetus ownership record = do not overwrite`.
//! Uninstall removes a path only when Impetus can prove ownership (path + owner + digest).

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{Arc, Mutex};
use thiserror::Error;

/// Stable identity of a resource Impetus created or claimed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipRecord {
    /// Absolute path key for the managed resource.
    pub path: String,
    /// Owning principal (e.g. `impetus`, extension id).
    pub owner: String,
    /// Provenance of the install (URI, manifest id, plan id).
    pub source: String,
    /// Content digest (typically SHA-256 hex).
    pub digest: String,
    /// Declared version of the installed resource.
    pub version: String,
    /// Stable id for the install operation that created this record.
    pub installation_id: String,
}

#[derive(Debug, Error)]
pub enum OwnershipError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("destination exists without matching ownership record: {0}")]
    UnownedDestination(String),
    #[error("ownership record already exists for path: {0}")]
    AlreadyExists(String),
    #[error("no ownership record for path: {0}")]
    NotOwned(String),
    #[error("ownership owner mismatch for path: {0}")]
    OwnerMismatch(String),
    #[error("ownership digest mismatch for path: {0}")]
    DigestMismatch(String),
}

/// SQLite-backed ownership store.
pub struct OwnershipStore {
    conn: Arc<Mutex<Connection>>,
}

impl OwnershipStore {
    /// Open or create the ownership database at `db_path`.
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self, OwnershipError> {
        let db_path = db_path.as_ref();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(db_path)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS ownership_records (
                path TEXT PRIMARY KEY NOT NULL,
                owner TEXT NOT NULL,
                source TEXT NOT NULL,
                digest TEXT NOT NULL,
                version TEXT NOT NULL,
                installation_id TEXT NOT NULL,
                created_unix_ms INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_ownership_installation
             ON ownership_records(installation_id)",
            [],
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Persist a new ownership record. Fails if a record for `path` already exists.
    pub fn create(&self, record: &OwnershipRecord) -> Result<(), OwnershipError> {
        let conn = self.conn.lock().expect("ownership db lock");
        let created_unix_ms = now_unix_ms();
        match conn.execute(
            "INSERT INTO ownership_records
                (path, owner, source, digest, version, installation_id, created_unix_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &record.path,
                &record.owner,
                &record.source,
                &record.digest,
                &record.version,
                &record.installation_id,
                created_unix_ms as i64,
            ],
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(OwnershipError::AlreadyExists(record.path.clone()))
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Lookup ownership by absolute path key.
    pub fn get_by_path(&self, path: &str) -> Result<Option<OwnershipRecord>, OwnershipError> {
        let conn = self.conn.lock().expect("ownership db lock");
        let row = conn
            .query_row(
                "SELECT path, owner, source, digest, version, installation_id
                 FROM ownership_records WHERE path = ?1",
                params![path],
                |row| {
                    Ok(OwnershipRecord {
                        path: row.get(0)?,
                        owner: row.get(1)?,
                        source: row.get(2)?,
                        digest: row.get(3)?,
                        version: row.get(4)?,
                        installation_id: row.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Refuse writes when the destination exists on disk without a matching record.
    ///
    /// Matching = a stored ownership row whose path key equals the normalized destination.
    pub fn ensure_can_overwrite(&self, destination: &Path) -> Result<(), OwnershipError> {
        if !destination.exists() {
            return Ok(());
        }

        let key = path_key(destination)?;
        match self.get_by_path(&key)? {
            Some(_) => Ok(()),
            None => Err(OwnershipError::UnownedDestination(key)),
        }
    }

    /// Remove a managed path only when Impetus can prove it owns the on-disk resource.
    ///
    /// Proof requires a stored record for `path` whose `owner` equals `expected_owner`
    /// and whose `digest` equals the SHA-256 digest of the current file bytes.
    /// Without that proof the path is left untouched.
    pub fn uninstall(&self, path: &Path, expected_owner: &str) -> Result<(), OwnershipError> {
        let key = path_key(path)?;
        let record = match self.get_by_path(&key)? {
            Some(record) => record,
            None => return Err(OwnershipError::NotOwned(key)),
        };

        if record.owner != expected_owner {
            return Err(OwnershipError::OwnerMismatch(key));
        }

        if !path.exists() {
            self.delete_record(&key)?;
            return Ok(());
        }

        let bytes = std::fs::read(path)?;
        let actual = content_digest(&bytes);
        if actual != record.digest {
            return Err(OwnershipError::DigestMismatch(key));
        }

        std::fs::remove_file(path)?;
        self.delete_record(&key)?;
        Ok(())
    }

    fn delete_record(&self, path_key: &str) -> Result<(), OwnershipError> {
        let conn = self.conn.lock().expect("ownership db lock");
        conn.execute(
            "DELETE FROM ownership_records WHERE path = ?1",
            params![path_key],
        )?;
        Ok(())
    }
}

/// Normalize a filesystem path into the store key (absolute; canonical when present).
pub fn path_key(path: &Path) -> Result<String, OwnershipError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let key = absolute.canonicalize().unwrap_or(absolute);
    Ok(key.to_string_lossy().into_owned())
}

/// Content digest stored on ownership records (`sha256:` + hex).
pub fn content_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha256:{hex}")
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, OwnershipStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = OwnershipStore::open(dir.path().join("ownership.db")).expect("open store");
        (dir, store)
    }

    fn sample_record(path: &Path) -> OwnershipRecord {
        let bytes = std::fs::read(path).unwrap_or_default();
        OwnershipRecord {
            path: path_key(path).expect("path key"),
            owner: "impetus".into(),
            source: "extension://demo/skill".into(),
            digest: content_digest(&bytes),
            version: "1.0.0".into(),
            installation_id: "install-001".into(),
        }
    }

    #[test]
    fn create_and_lookup_by_path() {
        let (dir, store) = temp_store();
        let dest = dir.path().join("managed.txt");
        std::fs::write(&dest, b"owned").expect("write dest");
        let record = sample_record(&dest);

        store.create(&record).expect("create");
        let found = store
            .get_by_path(&record.path)
            .expect("lookup")
            .expect("present");
        assert_eq!(found, record);
    }

    #[test]
    fn create_refuses_duplicate_path() {
        let (dir, store) = temp_store();
        let dest = dir.path().join("managed.txt");
        std::fs::write(&dest, b"owned").expect("write dest");
        let record = sample_record(&dest);

        store.create(&record).expect("create");
        let err = store.create(&record).expect_err("duplicate");
        assert!(matches!(err, OwnershipError::AlreadyExists(_)));
    }

    #[test]
    fn refuses_overwrite_when_destination_exists_without_record() {
        let (dir, store) = temp_store();
        let dest = dir.path().join("user-file.txt");
        std::fs::write(&dest, b"pre-existing user content").expect("write dest");

        let err = store
            .ensure_can_overwrite(&dest)
            .expect_err("must refuse unowned destination");
        assert!(matches!(err, OwnershipError::UnownedDestination(_)));
    }

    #[test]
    fn allows_overwrite_when_matching_record_exists() {
        let (dir, store) = temp_store();
        let dest = dir.path().join("managed.txt");
        std::fs::write(&dest, b"owned").expect("write dest");
        let record = sample_record(&dest);
        store.create(&record).expect("create");

        store
            .ensure_can_overwrite(&dest)
            .expect("matching record allows overwrite");
    }

    #[test]
    fn allows_write_when_destination_missing() {
        let (dir, store) = temp_store();
        let dest = dir.path().join("new-file.txt");
        assert!(!dest.exists());

        store
            .ensure_can_overwrite(&dest)
            .expect("missing destination is writable");
    }

    #[test]
    fn records_survive_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("ownership.db");
        let dest = dir.path().join("managed.txt");
        std::fs::write(&dest, b"owned").expect("write dest");
        let record = sample_record(&dest);

        {
            let store = OwnershipStore::open(&db).expect("open");
            store.create(&record).expect("create");
        }

        let reopened = OwnershipStore::open(&db).expect("reopen");
        let found = reopened
            .get_by_path(&record.path)
            .expect("lookup")
            .expect("present");
        assert_eq!(found, record);
    }

    #[test]
    fn uninstall_removes_owned_path_and_record() {
        let (dir, store) = temp_store();
        let dest = dir.path().join("managed.txt");
        std::fs::write(&dest, b"owned-content").expect("write dest");
        let record = sample_record(&dest);
        store.create(&record).expect("create");

        store.uninstall(&dest, "impetus").expect("owned uninstall");
        assert!(!dest.exists(), "owned path must be removed");
        assert!(
            store.get_by_path(&record.path).expect("lookup").is_none(),
            "ownership record must be cleared"
        );
    }

    #[test]
    fn uninstall_leaves_unowned_preexisting_path_untouched() {
        let (dir, store) = temp_store();
        let dest = dir.path().join("user-file.txt");
        std::fs::write(&dest, b"pre-existing user content").expect("write dest");

        let err = store
            .uninstall(&dest, "impetus")
            .expect_err("must refuse unowned path");
        assert!(matches!(err, OwnershipError::NotOwned(_)));
        assert!(dest.exists(), "unowned path must stay on disk");
        assert_eq!(
            std::fs::read_to_string(&dest).expect("read"),
            "pre-existing user content"
        );
    }

    #[test]
    fn uninstall_refuses_digest_mismatch_without_deleting() {
        let (dir, store) = temp_store();
        let dest = dir.path().join("managed.txt");
        std::fs::write(&dest, b"owned").expect("write dest");
        let mut record = sample_record(&dest);
        record.digest = content_digest(b"different-bytes");
        store.create(&record).expect("create");

        let err = store
            .uninstall(&dest, "impetus")
            .expect_err("digest mismatch");
        assert!(matches!(err, OwnershipError::DigestMismatch(_)));
        assert!(dest.exists(), "mismatch must not delete file");
        assert!(
            store.get_by_path(&record.path).expect("lookup").is_some(),
            "mismatch must keep ownership record"
        );
    }

    #[test]
    fn uninstall_refuses_owner_mismatch_without_deleting() {
        let (dir, store) = temp_store();
        let dest = dir.path().join("managed.txt");
        std::fs::write(&dest, b"owned").expect("write dest");
        let record = sample_record(&dest);
        store.create(&record).expect("create");

        let err = store
            .uninstall(&dest, "other-owner")
            .expect_err("owner mismatch");
        assert!(matches!(err, OwnershipError::OwnerMismatch(_)));
        assert!(dest.exists(), "owner mismatch must not delete file");
    }
}
