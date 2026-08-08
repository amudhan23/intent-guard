use serde::{Deserialize, Serialize};

/// What the user actually asked for. This is the context Rain never sees.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Task {
    pub destination: String,
    pub max_budget: f64,
    pub purpose: String,
}

/// What the agent wants to do next, before any money moves.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProposedAction {
    pub task_id: String,
    pub destination: String,
    pub amount: f64,
    pub description: String,
}

/// The verdict IntentGuard renders on a proposed action.
///
/// `status` is one of `"approved"`, `"blocked"`, `"escalated"`.
#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub status: String,
    pub reason: String,
}

impl Decision {
    pub fn approved(reason: impl Into<String>) -> Self {
        Self {
            status: "approved".to_string(),
            reason: reason.into(),
        }
    }

    pub fn blocked(reason: impl Into<String>) -> Self {
        Self {
            status: "blocked".to_string(),
            reason: reason.into(),
        }
    }

    pub fn escalated(reason: impl Into<String>) -> Self {
        Self {
            status: "escalated".to_string(),
            reason: reason.into(),
        }
    }

    pub fn is_approved(&self) -> bool {
        self.status == "approved"
    }
}
