//! Governed instruction references — operator-curated policy surface.
//!
//! Wire types live in `impetus-protocol`; runtime path helper stays here.

use std::path::{Path, PathBuf};

pub use impetus_protocol::{
    GovernedInstructionRef, POLICY_STORE_VERSION, PolicyStore, PolicyStoreError,
};

/// Conventional path under a daemon data root.
pub fn default_policy_store_path(data_root: &Path) -> PathBuf {
    data_root.join("policy_store.json")
}
