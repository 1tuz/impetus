//! Durable SSH approval storage in SQLite.
//!
//! Host-key approvals survive process restart by persisting in the same SQLite
//! database used for events and sessions.

use super::profile::HostKeyFingerprint;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SSHApprovalStoreError {
    #[error("storage lock poisoned")]
    Poisoned,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("approval not found for {host}:{port} user={user}")]
    NotFound {
        host: String,
        port: u16,
        user: String,
    },
}

/// Durable SSH host-key approval record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SSHApproval {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub host_key_fingerprint: HostKeyFingerprint,
    pub approved_at_unix_ms: u64,
}

/// Trait for SSH approval storage backends.
#[async_trait::async_trait]
pub trait SSHApprovalStore: Send + Sync {
    /// Save a host-key approval durably.
    async fn save_approval(&self, approval: &SSHApproval) -> Result<(), SSHApprovalStoreError>;

    /// Retrieve an existing approval by host, port, and user.
    async fn get_approval(
        &self,
        host: &str,
        port: u16,
        user: &str,
    ) -> Result<Option<SSHApproval>, SSHApprovalStoreError>;

    /// List all approved SSH connections.
    async fn list_approvals(&self) -> Result<Vec<SSHApproval>, SSHApprovalStoreError>;

    /// Revoke an approval (remove from storage).
    async fn revoke_approval(
        &self,
        host: &str,
        port: u16,
        user: &str,
    ) -> Result<(), SSHApprovalStoreError>;
}

/// SQLite-backed SSH approval store.
/// Shares the same database with event storage for consistency.
pub struct SqliteSSHApprovalStore {
    connection: Mutex<Connection>,
}

impl SqliteSSHApprovalStore {
    /// Open or create SSH approval store at the given database path.
    pub fn open(path: impl AsRef<Path>) -> Result<Arc<Self>, SSHApprovalStoreError> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS ssh_approvals (
                 host TEXT NOT NULL,
                 port INTEGER NOT NULL,
                 user TEXT NOT NULL,
                 host_key_fingerprint TEXT NOT NULL,
                 approved_at_unix_ms INTEGER NOT NULL,
                 PRIMARY KEY (host, port, user)
             );",
        )?;

        Ok(Arc::new(Self {
            connection: Mutex::new(connection),
        }))
    }
}

#[async_trait::async_trait]
impl SSHApprovalStore for SqliteSSHApprovalStore {
    async fn save_approval(&self, approval: &SSHApproval) -> Result<(), SSHApprovalStoreError> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| SSHApprovalStoreError::Poisoned)?;

        conn.execute(
            "INSERT OR REPLACE INTO ssh_approvals 
             (host, port, user, host_key_fingerprint, approved_at_unix_ms) 
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                &approval.host,
                approval.port,
                &approval.user,
                approval.host_key_fingerprint.as_str(),
                approval.approved_at_unix_ms,
            ],
        )?;

        Ok(())
    }

    async fn get_approval(
        &self,
        host: &str,
        port: u16,
        user: &str,
    ) -> Result<Option<SSHApproval>, SSHApprovalStoreError> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| SSHApprovalStoreError::Poisoned)?;

        let result = conn.query_row(
            "SELECT host, port, user, host_key_fingerprint, approved_at_unix_ms 
             FROM ssh_approvals 
             WHERE host = ?1 AND port = ?2 AND user = ?3",
            params![host, port, user],
            |row| {
                Ok(SSHApproval {
                    host: row.get(0)?,
                    port: row.get(1)?,
                    user: row.get(2)?,
                    host_key_fingerprint: HostKeyFingerprint::from_public_key(
                        row.get::<_, String>(3)?.as_bytes(),
                    ),
                    approved_at_unix_ms: row.get(4)?,
                })
            },
        );

        match result {
            Ok(approval) => Ok(Some(approval)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn list_approvals(&self) -> Result<Vec<SSHApproval>, SSHApprovalStoreError> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| SSHApprovalStoreError::Poisoned)?;

        let mut stmt = conn.prepare(
            "SELECT host, port, user, host_key_fingerprint, approved_at_unix_ms 
             FROM ssh_approvals 
             ORDER BY approved_at_unix_ms DESC",
        )?;

        let approvals = stmt
            .query_map([], |row| {
                Ok(SSHApproval {
                    host: row.get(0)?,
                    port: row.get(1)?,
                    user: row.get(2)?,
                    host_key_fingerprint: HostKeyFingerprint::from_public_key(
                        row.get::<_, String>(3)?.as_bytes(),
                    ),
                    approved_at_unix_ms: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(approvals)
    }

    async fn revoke_approval(
        &self,
        host: &str,
        port: u16,
        user: &str,
    ) -> Result<(), SSHApprovalStoreError> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| SSHApprovalStoreError::Poisoned)?;

        let deleted = conn.execute(
            "DELETE FROM ssh_approvals WHERE host = ?1 AND port = ?2 AND user = ?3",
            params![host, port, user],
        )?;

        if deleted == 0 {
            return Err(SSHApprovalStoreError::NotFound {
                host: host.to_string(),
                port,
                user: user.to_string(),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    async fn test_store() -> Arc<SqliteSSHApprovalStore> {
        let db_path = std::env::temp_dir().join(format!("ssh-approvals-{}.db", Uuid::new_v4()));
        SqliteSSHApprovalStore::open(&db_path).expect("create test store")
    }

    fn test_approval() -> SSHApproval {
        SSHApproval {
            host: "example.com".into(),
            port: 22,
            user: "testuser".into(),
            host_key_fingerprint: HostKeyFingerprint::from_public_key(b"test-key"),
            approved_at_unix_ms: 1234567890,
        }
    }

    #[tokio::test]
    async fn save_and_retrieve_approval() {
        let store = test_store().await;
        let approval = test_approval();

        store.save_approval(&approval).await.expect("save approval");

        let retrieved = store
            .get_approval("example.com", 22, "testuser")
            .await
            .expect("get approval")
            .expect("approval exists");

        assert_eq!(retrieved.host, approval.host);
        assert_eq!(retrieved.port, approval.port);
        assert_eq!(retrieved.user, approval.user);
    }

    #[tokio::test]
    async fn get_nonexistent_approval_returns_none() {
        let store = test_store().await;

        let result = store
            .get_approval("nonexistent.com", 22, "nobody")
            .await
            .expect("query succeeds");

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn save_replaces_existing_approval() {
        let store = test_store().await;
        let mut approval = test_approval();

        store
            .save_approval(&approval)
            .await
            .expect("save first approval");

        // Update fingerprint
        approval.host_key_fingerprint = HostKeyFingerprint::from_public_key(b"new-key");
        approval.approved_at_unix_ms = 9999999999;

        store
            .save_approval(&approval)
            .await
            .expect("save updated approval");

        let retrieved = store
            .get_approval("example.com", 22, "testuser")
            .await
            .expect("get approval")
            .expect("approval exists");

        assert_eq!(retrieved.approved_at_unix_ms, 9999999999);
    }

    #[tokio::test]
    async fn list_approvals_returns_all() {
        let store = test_store().await;

        let approval1 = SSHApproval {
            host: "host1.com".into(),
            port: 22,
            user: "user1".into(),
            host_key_fingerprint: HostKeyFingerprint::from_public_key(b"key1"),
            approved_at_unix_ms: 1000,
        };

        let approval2 = SSHApproval {
            host: "host2.com".into(),
            port: 2222,
            user: "user2".into(),
            host_key_fingerprint: HostKeyFingerprint::from_public_key(b"key2"),
            approved_at_unix_ms: 2000,
        };

        store.save_approval(&approval1).await.expect("save 1");
        store.save_approval(&approval2).await.expect("save 2");

        let all = store.list_approvals().await.expect("list approvals");

        assert_eq!(all.len(), 2);
        // Should be ordered by approved_at_unix_ms DESC
        assert_eq!(all[0].host, "host2.com");
        assert_eq!(all[1].host, "host1.com");
    }

    #[tokio::test]
    async fn revoke_approval_removes_from_storage() {
        let store = test_store().await;
        let approval = test_approval();

        store.save_approval(&approval).await.expect("save approval");

        store
            .revoke_approval("example.com", 22, "testuser")
            .await
            .expect("revoke approval");

        let result = store
            .get_approval("example.com", 22, "testuser")
            .await
            .expect("query succeeds");

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn revoke_nonexistent_approval_returns_error() {
        let store = test_store().await;

        let result = store.revoke_approval("nonexistent.com", 22, "nobody").await;

        assert!(matches!(
            result,
            Err(SSHApprovalStoreError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn approval_survives_store_reload() {
        let db_path = std::env::temp_dir().join(format!("ssh-approvals-{}.db", Uuid::new_v4()));

        {
            let store = SqliteSSHApprovalStore::open(&db_path).expect("create store");
            let approval = test_approval();
            store.save_approval(&approval).await.expect("save");
        }

        // Reload from same database
        let store = SqliteSSHApprovalStore::open(&db_path).expect("reload store");
        let retrieved = store
            .get_approval("example.com", 22, "testuser")
            .await
            .expect("get approval")
            .expect("approval persisted");

        assert_eq!(retrieved.host, "example.com");
        assert_eq!(retrieved.user, "testuser");
    }
}
