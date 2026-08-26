use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::policy::Action;

pub type ApprovalId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ApprovalState {
    Pending,
    Approved,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub id: ApprovalId,
    pub action: Action,
    pub reason: String,
    pub state: ApprovalState,
}

impl ApprovalRequest {
    pub fn pending(action: Action, reason: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            action,
            reason,
            state: ApprovalState::Pending,
        }
    }
}
