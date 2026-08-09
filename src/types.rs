use serde::{Deserialize, Serialize};

/// What the user actually asked for. This is the context Rain never sees.
///
/// Dates are ISO-8601 `YYYY-MM-DD` strings. That format is chosen so ordinary
/// string comparison is also chronological comparison, which is the whole
/// reason this type needs no date library: `"2026-08-15" <= "2026-08-16"`
/// sorts correctly because the fields run widest-first and are zero-padded.
/// Whatever produces a `Task` is responsible for enforcing that shape —
/// see `agent::parse_task_from_prompt`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Task {
    pub destination: String,
    pub max_budget: f64,
    pub purpose: String,
    /// First day of the travel window the user stated, e.g. `"2026-08-15"`.
    pub start_date: String,
    /// Last day of that window, e.g. `"2026-08-16"`.
    pub end_date: String,
}

/// What the agent wants to do next, before any money moves.
///
/// The travel dates are their own fields rather than prose inside
/// `description` because `intent_check` has to compare them. A date buried in
/// free text can be displayed but not validated, which is exactly the gap
/// these two fields close.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProposedAction {
    pub task_id: String,
    pub destination: String,
    pub amount: f64,
    pub description: String,
    /// Departure date, ISO-8601 `YYYY-MM-DD`.
    pub start_date: String,
    /// Return date, ISO-8601 `YYYY-MM-DD`.
    pub end_date: String,
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
