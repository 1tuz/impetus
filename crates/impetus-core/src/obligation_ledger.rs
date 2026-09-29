//! Lightweight promise / obligation ledger.
//!
//! Tracks model-declared commitments (open TODOs, promised docs/tests, etc.)
//! so [`crate::CompletionGate`] can fail-closed when required items remain
//! [`ObligationStatus::Open`]. Not a knowledge graph — id/kind/summary/status
//! only; never secrets.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Lifecycle of a tracked obligation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObligationStatus {
    Open,
    Fulfilled,
    Cancelled,
}

impl ObligationStatus {
    pub fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }
}

/// One tracked commitment. Summaries/labels only — no tokens or private keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Obligation {
    pub id: String,
    /// Stable kind token (e.g. `promised_todo`, `docs_update`, `acceptance`).
    pub kind: String,
    pub summary: String,
    pub status: ObligationStatus,
}

/// Errors from ledger mutations.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ObligationLedgerError {
    #[error("obligation `{0}` not found")]
    NotFound(String),
    #[error("obligation `{0}` is not open (status={1:?})")]
    NotOpen(String, ObligationStatus),
    #[error("obligation `{0}` already exists")]
    Duplicate(String),
}

/// In-memory per-session ledger of unresolved commitments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObligationLedger {
    items: Vec<Obligation>,
}

impl ObligationLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn as_slice(&self) -> &[Obligation] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Obligation> {
        self.items.iter().find(|o| o.id == id)
    }

    /// Obligations still [`ObligationStatus::Open`] — these block Accepted.
    pub fn open_required(&self) -> Vec<&Obligation> {
        self.items.iter().filter(|o| o.status.is_open()).collect()
    }

    pub fn has_open_required(&self) -> bool {
        self.items.iter().any(|o| o.status.is_open())
    }

    /// Missing tokens for [`crate::CompletionVerdict::Insufficient`].
    pub fn missing_labels(&self) -> Vec<String> {
        self.open_required()
            .into_iter()
            .map(|o| format!("obligation:{}", o.id))
            .collect()
    }

    /// Register with a caller-supplied id.
    pub fn register(
        &mut self,
        id: impl Into<String>,
        kind: impl Into<String>,
        summary: impl Into<String>,
    ) -> Result<&Obligation, ObligationLedgerError> {
        let id = id.into();
        if self.items.iter().any(|o| o.id == id) {
            return Err(ObligationLedgerError::Duplicate(id));
        }
        self.items.push(Obligation {
            id,
            kind: kind.into(),
            summary: summary.into(),
            status: ObligationStatus::Open,
        });
        Ok(self.items.last().expect("just pushed"))
    }

    /// Register with a fresh UUID id.
    pub fn register_new(
        &mut self,
        kind: impl Into<String>,
        summary: impl Into<String>,
    ) -> &Obligation {
        let id = Uuid::new_v4().to_string();
        self.register(id, kind, summary)
            .expect("fresh uuid cannot duplicate")
    }

    pub fn fulfill(&mut self, id: &str) -> Result<&Obligation, ObligationLedgerError> {
        self.set_status(id, ObligationStatus::Fulfilled)
    }

    pub fn cancel(&mut self, id: &str) -> Result<&Obligation, ObligationLedgerError> {
        self.set_status(id, ObligationStatus::Cancelled)
    }

    fn set_status(
        &mut self,
        id: &str,
        status: ObligationStatus,
    ) -> Result<&Obligation, ObligationLedgerError> {
        let Some(item) = self.items.iter_mut().find(|o| o.id == id) else {
            return Err(ObligationLedgerError::NotFound(id.into()));
        };
        if !item.status.is_open() {
            return Err(ObligationLedgerError::NotOpen(id.into(), item.status));
        }
        item.status = status;
        Ok(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fulfill_clears_open_required() {
        let mut ledger = ObligationLedger::new();
        ledger
            .register("o1", "promised_todo", "wire tests")
            .expect("register");
        assert!(ledger.has_open_required());
        ledger.fulfill("o1").expect("fulfill");
        assert!(!ledger.has_open_required());
        assert!(ledger.missing_labels().is_empty());
    }

    #[test]
    fn cancel_also_clears_open() {
        let mut ledger = ObligationLedger::new();
        ledger
            .register("o1", "docs_update", "ARCHITECTURE")
            .expect("register");
        ledger.cancel("o1").expect("cancel");
        assert!(!ledger.has_open_required());
    }

    #[test]
    fn duplicate_id_rejected() {
        let mut ledger = ObligationLedger::new();
        ledger.register("o1", "k", "a").expect("first");
        assert!(matches!(
            ledger.register("o1", "k", "b"),
            Err(ObligationLedgerError::Duplicate(_))
        ));
    }
}
