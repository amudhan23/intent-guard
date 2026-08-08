use std::collections::HashMap;

use crate::types::{Decision, ProposedAction, Task};

/// Proof that a `ProposedAction` cleared intent checking.
///
/// The inner field is private to this module, so the only way to obtain one is
/// through [`evaluate`] returning [`Verdict::Approved`]. `rain_client` takes
/// this type — not a bare `ProposedAction` — which makes "call Rain on a
/// blocked action" a compile error rather than a code-review question.
#[derive(Debug)]
pub struct ApprovedAction<'a>(&'a ProposedAction);

impl<'a> ApprovedAction<'a> {
    pub fn action(&self) -> &'a ProposedAction {
        self.0
    }
}

/// Outcome of a full evaluation: either a capability to spend, or a refusal.
#[derive(Debug)]
pub enum Verdict<'a> {
    Approved {
        token: ApprovedAction<'a>,
        decision: Decision,
    },
    Denied(Decision),
}

impl<'a> Verdict<'a> {
    /// Uniform access to the decision regardless of arm. Used by the tests;
    /// `main` matches on the arms directly so it can reach the approval token.
    #[allow(dead_code)]
    pub fn decision(&self) -> &Decision {
        match self {
            Verdict::Approved { decision, .. } => decision,
            Verdict::Denied(decision) => decision,
        }
    }
}

/// Pure intent match: does this action fit the task the user described?
pub fn check_intent(task: &Task, action: &ProposedAction) -> Decision {
    if action.destination != task.destination {
        return Decision::blocked(format!(
            "destination mismatch: task targets '{}' but action goes to '{}'",
            task.destination, action.destination
        ));
    }

    if action.amount > task.max_budget {
        return Decision::blocked(format!(
            "over budget: action costs ${:.2} but task allows at most ${:.2}",
            action.amount, task.max_budget
        ));
    }

    Decision::approved(format!(
        "action matches task intent (destination '{}', ${:.2} within ${:.2} budget)",
        task.destination, action.amount, task.max_budget
    ))
}

/// Counts how many times an agent has taken a swing at the same task.
#[derive(Debug, Default)]
pub struct AttemptTracker {
    attempts: HashMap<String, u32>,
}

impl AttemptTracker {
    pub fn new() -> Self {
        Self {
            attempts: HashMap::new(),
        }
    }

    /// Records one more attempt on `task_id` and returns the new total.
    pub fn record_attempt(&mut self, task_id: &str) -> u32 {
        let counter = self.attempts.entry(task_id.to_string()).or_insert(0);
        *counter += 1;
        *counter
    }

    /// True once the agent has burned `threshold` or more attempts without resolving.
    pub fn is_stuck(&self, task_id: &str, threshold: u32) -> bool {
        self.attempts.get(task_id).copied().unwrap_or(0) >= threshold
    }

    /// Attempts recorded so far. Read by the tests and by callers that want to
    /// report the count without mutating it.
    #[allow(dead_code)]
    pub fn count(&self, task_id: &str) -> u32 {
        self.attempts.get(task_id).copied().unwrap_or(0)
    }
}

/// The one entry point that can mint an [`ApprovedAction`].
///
/// Records the attempt, escalates on non-convergence, then falls through to the
/// intent match. Only the approve path constructs the token.
pub fn evaluate<'a>(
    task: &Task,
    action: &'a ProposedAction,
    tracker: &mut AttemptTracker,
    stuck_threshold: u32,
) -> Verdict<'a> {
    let attempts = tracker.record_attempt(&action.task_id);

    if tracker.is_stuck(&action.task_id, stuck_threshold) {
        return Verdict::Denied(Decision::escalated(format!(
            "non-convergence: {} attempts on task '{}' (threshold {}) — handing back to the user",
            attempts, action.task_id, stuck_threshold
        )));
    }

    let decision = check_intent(task, action);
    if decision.is_approved() {
        Verdict::Approved {
            token: ApprovedAction(action),
            decision,
        }
    } else {
        Verdict::Denied(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> Task {
        Task {
            destination: "NYC".to_string(),
            max_budget: 500.0,
            purpose: "hackathon attendance".to_string(),
        }
    }

    fn action(destination: &str, amount: f64) -> ProposedAction {
        ProposedAction {
            task_id: "t-1".to_string(),
            destination: destination.to_string(),
            amount,
            description: "flight".to_string(),
        }
    }

    #[test]
    fn approves_action_matching_task() {
        let decision = check_intent(&task(), &action("NYC", 480.0));
        assert_eq!(decision.status, "approved");
    }

    #[test]
    fn blocks_destination_mismatch() {
        let decision = check_intent(&task(), &action("Miami", 450.0));
        assert_eq!(decision.status, "blocked");
        assert!(
            decision.reason.contains("destination mismatch"),
            "{}",
            decision.reason
        );
    }

    #[test]
    fn blocks_over_budget() {
        let decision = check_intent(&task(), &action("NYC", 750.0));
        assert_eq!(decision.status, "blocked");
        assert!(
            decision.reason.contains("over budget"),
            "{}",
            decision.reason
        );
    }

    #[test]
    fn budget_boundary_is_inclusive() {
        let decision = check_intent(&task(), &action("NYC", 500.0));
        assert_eq!(decision.status, "approved");
    }

    #[test]
    fn destination_check_precedes_budget_check() {
        let decision = check_intent(&task(), &action("Miami", 750.0));
        assert!(
            decision.reason.contains("destination mismatch"),
            "{}",
            decision.reason
        );
    }

    #[test]
    fn record_attempt_returns_running_count() {
        let mut tracker = AttemptTracker::new();
        assert_eq!(tracker.record_attempt("t-1"), 1);
        assert_eq!(tracker.record_attempt("t-1"), 2);
        assert_eq!(tracker.record_attempt("t-1"), 3);
    }

    #[test]
    fn tracks_task_ids_independently() {
        let mut tracker = AttemptTracker::new();
        tracker.record_attempt("t-1");
        tracker.record_attempt("t-1");
        tracker.record_attempt("t-2");
        assert_eq!(tracker.count("t-1"), 2);
        assert_eq!(tracker.count("t-2"), 1);
    }

    #[test]
    fn three_attempts_marks_task_stuck() {
        let mut tracker = AttemptTracker::new();
        tracker.record_attempt("t-1");
        assert!(!tracker.is_stuck("t-1", 3));
        tracker.record_attempt("t-1");
        assert!(!tracker.is_stuck("t-1", 3));
        tracker.record_attempt("t-1");
        assert!(tracker.is_stuck("t-1", 3));
    }

    #[test]
    fn unseen_task_is_never_stuck() {
        let tracker = AttemptTracker::new();
        assert!(!tracker.is_stuck("never-seen", 3));
    }

    #[test]
    fn evaluate_escalates_on_third_attempt() {
        let mut tracker = AttemptTracker::new();
        let task = task();
        let valid = action("NYC", 100.0);

        for expected in ["approved", "approved", "escalated"] {
            let verdict = evaluate(&task, &valid, &mut tracker, 3);
            assert_eq!(verdict.decision().status, expected);
        }
    }

    #[test]
    fn evaluate_denies_mismatch_without_minting_a_token() {
        let mut tracker = AttemptTracker::new();
        let wrong_city = action("Miami", 100.0);
        let verdict = evaluate(&task(), &wrong_city, &mut tracker, 3);
        assert!(matches!(verdict, Verdict::Denied(_)));
    }

    #[test]
    fn evaluate_mints_a_token_only_when_approved() {
        let mut tracker = AttemptTracker::new();
        let good = action("NYC", 100.0);
        match evaluate(&task(), &good, &mut tracker, 3) {
            Verdict::Approved { token, decision } => {
                assert_eq!(decision.status, "approved");
                assert_eq!(token.action().task_id, "t-1");
            }
            Verdict::Denied(d) => panic!("expected approval, got {}: {}", d.status, d.reason),
        }
    }
}
