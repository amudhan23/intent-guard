mod agent;
mod intent_check;
mod rain_client;
mod types;

use agent::Origin;
use intent_check::{AttemptTracker, Verdict, evaluate};
use rain_client::{DEFAULT_MCC, RainConfig, Settlement};
use types::{ProposedAction, Task};

/// Escalate once an agent has taken this many swings at the same task.
const STUCK_THRESHOLD: u32 = 3;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    banner();

    let config = RainConfig::from_env()?;

    println!("Funding sandbox collateral...");
    rain_client::fund_collateral(&config).await?;
    println!("  collateral funded\n");

    let task = Task {
        destination: "NYC".to_string(),
        max_budget: 500.0,
        purpose: "hackathon attendance".to_string(),
    };

    let mut tracker = AttemptTracker::new();

    scenario_one(&task, &mut tracker, &config).await;
    scenario_one_b(&task, &mut tracker, &config).await;
    scenario_two(&task, &mut tracker, &config).await;
    scenario_three(&task, &mut tracker, &config).await;

    println!("{}", "=".repeat(74));
    println!("Rain sees only payment data. IntentGuard sees the task.");
    println!("Blocked and escalated actions never reach the payment layer at all.");
    println!("{}", "=".repeat(74));

    Ok(())
}

// ------------------------------------------------------------- scenarios ----

async fn scenario_one(task: &Task, tracker: &mut AttemptTracker, config: &RainConfig) {
    header(1, "Clean approval");

    let action = ProposedAction {
        task_id: "scenario-1".to_string(),
        destination: "NYC".to_string(),
        amount: 480.0,
        description: "Delta flight to NYC".to_string(),
    };

    print_task(task);
    print_action(&action);
    run(task, &action, tracker, config, Settlement::Settle).await;
}

/// Scenario 1 again, except a real Claude agent writes the proposed action.
///
/// The gate does not care where the action came from: it runs the same
/// `evaluate` -> `execute_rain_flow` pipeline as every other scenario.
async fn scenario_one_b(task: &Task, tracker: &mut AttemptTracker, config: &RainConfig) {
    header_b(1, "Clean approval, proposed by a live agent");

    print_task(task);
    println!("  Asking a real Claude agent to propose a booking for this task...\n");

    let (action, origin) = agent::propose_or_fallback(task).await;

    match origin {
        Origin::Claude => println!("  The agent answered. Here is what it generated:\n"),
        Origin::Fallback(reason) => {
            println!("  FALLBACK: {reason}");
            println!("  Using the known-good hardcoded booking instead.\n");
        }
    }

    print_action(&action);
    run(task, &action, tracker, config, Settlement::Settle).await;
}

async fn scenario_two(task: &Task, tracker: &mut AttemptTracker, config: &RainConfig) {
    header(2, "Intent mismatch");

    let action = ProposedAction {
        task_id: "scenario-2".to_string(),
        destination: "Miami".to_string(),
        amount: 450.0,
        description: "Flight to Miami".to_string(),
    };

    print_task(task);
    print_action(&action);
    println!("  Note: Rain's Agent Control Layer would allow this — an airline");
    println!("        charge of $450.00 is under any sane per-transaction limit.");
    println!("        Only the task context reveals it is the wrong city.\n");
    run(task, &action, tracker, config, Settlement::Settle).await;
}

/// One booking the agent tries and does not land.
///
/// IntentGuard approves each of these — the destination and the price are
/// genuinely fine. The booking fails downstream instead, at the merchant, so
/// the Rain flow stops at authorization and captures nothing.
struct FailedAttempt {
    description: &'static str,
    amount: f64,
    /// Why the purchase fell through, printed after the Rain call.
    failure: &'static str,
    settlement: Settlement,
}

/// The attempts that actually reach Rain in scenario 3. Both are authorize-only:
/// one task must never produce more than zero completed bookings here.
const SCENARIO_3_ATTEMPTS: [FailedAttempt; 2] = [
    FailedAttempt {
        description: "JetBlue flight to NYC",
        amount: 460.0,
        failure: "flight sold out, retrying",
        settlement: Settlement::AuthorizeOnly,
    },
    FailedAttempt {
        description: "United flight to NYC",
        amount: 470.0,
        failure: "no seats available, retrying",
        settlement: Settlement::AuthorizeOnly,
    },
];

/// The booking the agent would have tried third. IntentGuard stops it first.
const SCENARIO_3_THIRD_TRY: (&str, f64) = ("American flight to NYC", 455.0);

fn scenario_three_action(description: &str, amount: f64) -> ProposedAction {
    ProposedAction {
        task_id: "scenario-3".to_string(),
        destination: "NYC".to_string(),
        amount,
        description: description.to_string(),
    }
}

async fn scenario_three(task: &Task, tracker: &mut AttemptTracker, config: &RainConfig) {
    header(3, "Non-convergence");

    print_task(task);
    println!("  The agent keeps trying to book this trip and keeps failing. Each");
    println!("  action it proposes is individually valid; the pattern is the problem.");
    println!("  Nothing here completes a charge — the first two attempts authorize");
    println!("  and are never captured, and the third never reaches Rain at all.\n");

    for (index, attempt) in SCENARIO_3_ATTEMPTS.iter().enumerate() {
        let action = scenario_three_action(attempt.description, attempt.amount);

        println!("  --- attempt {} ---", index + 1);
        print_action(&action);
        run(task, &action, tracker, config, attempt.settlement).await;
        println!("  BOOKING FAILED: {}\n", attempt.failure);
    }

    let (description, amount) = SCENARIO_3_THIRD_TRY;
    let action = scenario_three_action(description, amount);

    println!("  --- attempt {} ---", SCENARIO_3_ATTEMPTS.len() + 1);
    print_action(&action);
    println!(
        "  Pattern detected: {} failed attempts on this task. IntentGuard",
        SCENARIO_3_ATTEMPTS.len()
    );
    println!("  escalates to human review rather than letting the agent try again.\n");
    run(task, &action, tracker, config, Settlement::Settle).await;
}

// --------------------------------------------------------------- plumbing ---

/// The single gate. `execute_rain_flow` takes the approval token that only
/// `Verdict::Approved` carries, so the denied arm below has no way to spend.
async fn run(
    task: &Task,
    action: &ProposedAction,
    tracker: &mut AttemptTracker,
    config: &RainConfig,
    settlement: Settlement,
) {
    match evaluate(task, action, tracker, STUCK_THRESHOLD) {
        Verdict::Approved { token, decision } => {
            print_decision(&decision);
            println!("  Proceeding to Rain...");

            match rain_client::execute_rain_flow(&token, config, DEFAULT_MCC, settlement).await {
                Ok(result) => {
                    match settlement {
                        Settlement::Settle => println!("  RAIN CARD ISSUED AND CHARGE SETTLED"),
                        Settlement::AuthorizeOnly => {
                            println!("  RAIN CARD ISSUED AND AUTHORIZATION APPROVED");
                            println!("  Authorization succeeded but settlement was deliberately");
                            println!(
                                "  skipped — flight unavailable, agent must retry. No money moved."
                            );
                        }
                    }
                    println!("    card_id        : {}", result.card_id);
                    println!("    transaction_id : {}", result.transaction_id);
                    println!("    status         : {}", result.status);
                }
                Err(e) => {
                    println!("  RAIN CALL FAILED: {e}");
                }
            }
        }
        Verdict::Denied(decision) => {
            print_decision(&decision);
            println!("  No Rain API calls made — request blocked before reaching payment layer");
        }
    }
    println!();
}

// ----------------------------------------------------------------- output ---

fn banner() {
    println!();
    println!("{}", "=".repeat(74));
    println!("  IntentGuard — intent validation before Rain's payment layer");
    println!("{}", "=".repeat(74));
    println!();
}

fn header(number: u8, title: &str) {
    println!("{}", "-".repeat(74));
    println!("  SCENARIO {number} — {title}");
    println!("{}", "-".repeat(74));
}

fn header_b(number: u8, title: &str) {
    println!("{}", "-".repeat(74));
    println!("  SCENARIO {number}B — {title}");
    println!("{}", "-".repeat(74));
}

fn print_task(task: &Task) {
    println!("  TASK");
    println!("    destination : {}", task.destination);
    println!("    max_budget  : ${:.2}", task.max_budget);
    println!("    purpose     : {}", task.purpose);
    println!();
}

fn print_action(action: &ProposedAction) {
    println!("  PROPOSED ACTION");
    println!("    task_id     : {}", action.task_id);
    println!("    destination : {}", action.destination);
    println!("    amount      : ${:.2}", action.amount);
    println!("    description : {}", action.description);
    println!();
}

fn print_decision(decision: &types::Decision) {
    println!("  DECISION    : {}", decision.status.to_uppercase());
    println!("  REASON      : {}", decision.reason);
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

    /// The double-booking regression: one task, no completed charges. Every
    /// scenario-3 attempt that reaches Rain must stop at authorization.
    #[test]
    fn no_scenario_three_attempt_is_ever_settled() {
        assert!(
            SCENARIO_3_ATTEMPTS
                .iter()
                .all(|attempt| attempt.settlement == Settlement::AuthorizeOnly)
        );
    }

    /// The attempts that reach Rain are approved on the merits — they fail at
    /// the merchant, not at IntentGuard — and the next one is escalated before
    /// any Rain call can happen.
    #[test]
    fn scenario_three_escalates_before_a_third_rain_call() {
        let task = task();
        let mut tracker = AttemptTracker::new();

        for attempt in SCENARIO_3_ATTEMPTS.iter() {
            let action = scenario_three_action(attempt.description, attempt.amount);
            let verdict = evaluate(&task, &action, &mut tracker, STUCK_THRESHOLD);
            assert!(
                matches!(verdict, Verdict::Approved { .. }),
                "{} should clear intent checking, got {}",
                attempt.description,
                verdict.decision().reason
            );
        }

        let (description, amount) = SCENARIO_3_THIRD_TRY;
        let action = scenario_three_action(description, amount);
        let verdict = evaluate(&task, &action, &mut tracker, STUCK_THRESHOLD);

        assert!(
            matches!(verdict, Verdict::Denied(_)),
            "third attempt must not mint an approval token"
        );
        assert_eq!(verdict.decision().status, "escalated");
    }

    /// Guards the narrative: the demo claims two failed attempts trigger the
    /// escalation, which only holds while the plan and the threshold agree.
    #[test]
    fn attempt_plan_matches_the_stuck_threshold() {
        assert_eq!(SCENARIO_3_ATTEMPTS.len() as u32 + 1, STUCK_THRESHOLD);
    }
}
