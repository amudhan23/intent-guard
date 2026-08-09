mod agent;
mod intent_check;
mod monad_client;
mod rain_client;
mod types;

use agent::Origin;
use intent_check::{AttemptTracker, Verdict, evaluate};
use monad_client::MonadConfig;
use rain_client::{DEFAULT_MCC, RainConfig, Settlement};
use types::{Decision, ProposedAction, Task};

/// Escalate once an agent has taken this many swings at the same task.
const STUCK_THRESHOLD: u32 = 3;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    banner();

    let config = RainConfig::from_env()?;

    // The audit log is optional by design. A missing key or an unreachable
    // testnet degrades the demo to exactly what it was before Monad existed,
    // rather than stopping a decision from being made.
    let monad = match MonadConfig::from_env() {
        Ok(monad) => {
            println!(
                "Monad audit log active — logging decisions from {}",
                monad.address()
            );
            println!();
            Some(monad)
        }
        Err(e) => {
            println!("Monad audit log disabled: {e}");
            println!("Decisions are still made and enforced; they just are not anchored on chain.");
            println!();
            None
        }
    };
    let monad = monad.as_ref();

    println!("Funding sandbox collateral...");
    rain_client::fund_collateral(&config).await?;
    println!("  collateral funded\n");

    let task = Task {
        destination: "NYC".to_string(),
        max_budget: 500.0,
        purpose: "hackathon attendance".to_string(),
    };

    let mut tracker = AttemptTracker::new();

    scenario_zero(&mut tracker, &config, monad).await;
    scenario_one(&task, &mut tracker, &config, monad).await;
    scenario_one_b(&task, &mut tracker, &config, monad).await;
    scenario_two(&task, &mut tracker, &config, monad).await;
    scenario_three(&task, &mut tracker, &config, monad).await;

    verify_one_decision_on_chain(monad).await;

    println!("{}", "=".repeat(74));
    println!("Rain sees only payment data. IntentGuard sees the task.");
    println!("Blocked and escalated actions never reach the payment layer at all.");
    println!("{}", "=".repeat(74));

    Ok(())
}

// ------------------------------------------------------------- scenarios ----

/// The request used when nobody types one. The parsing is still live — only
/// the sentence is fixed, so a scripted run has something reliable to show.
const EXAMPLE_REQUEST: &str = "I need to get to New York City for a hackathon happening \
     August 15th and 16th. My budget is $500. This is for me, traveling alone.";

/// Scenario 0's task id. Separate from scenario 1B's so the two live scenarios
/// are tracked as the distinct tasks they are.
const SCENARIO_ZERO_TASK_ID: &str = "scenario-0";

/// Reads a request from stdin when `INTENTGUARD_INTERACTIVE=1`, so a judge can
/// type their own. Falls back to the example on an unset flag or empty input.
fn live_request() -> String {
    if std::env::var("INTENTGUARD_INTERACTIVE").as_deref() != Ok("1") {
        return EXAMPLE_REQUEST.to_string();
    }

    println!("  Where do you want to go, and what is your budget?");
    print!("  > ");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let mut typed = String::new();
    match std::io::stdin().read_line(&mut typed) {
        Ok(_) if !typed.trim().is_empty() => typed.trim().to_string(),
        _ => {
            println!("  (nothing typed — using the example request)");
            EXAMPLE_REQUEST.to_string()
        }
    }
}

/// End to end with nothing hardcoded but the sentence: plain English becomes a
/// task, an agent proposes a booking for it, and IntentGuard rules on it.
///
/// Because neither the task nor the action is scripted, this can legitimately
/// come out blocked. That is not a failure of the demo — it is the demo.
async fn scenario_zero(
    tracker: &mut AttemptTracker,
    config: &RainConfig,
    monad: Option<&MonadConfig>,
) {
    header(
        0,
        "Fully live: natural language -> task -> agent -> decision",
    );

    let request = live_request();
    println!("  User request: {request}\n");

    println!("  Parsing task from natural language...");
    let task = match agent::parse_task_from_prompt(&request).await {
        Ok(task) => task,
        Err(e) => {
            println!("  COULD NOT PARSE A VALID TASK: {e}");
            println!("  In production this would prompt the user to clarify.");
            println!("  Skipping to the scripted scenarios.\n");
            return;
        }
    };

    println!("  Claude extracted this task — none of it is hardcoded:");
    print_task(&task);

    println!("  Asking a real Claude agent to propose a booking for this task...\n");

    // No fallback here, unlike scenario 1B: its hardcoded NYC booking would be
    // the wrong answer for whatever task the user actually described.
    let mut action = match agent::agent_propose_action(&task).await {
        Ok(action) => action,
        Err(e) => {
            println!("  THE AGENT DID NOT PROPOSE ANYTHING: {e}");
            println!("  Skipping to the scripted scenarios.\n");
            return;
        }
    };
    action.task_id = SCENARIO_ZERO_TASK_ID.to_string();

    print_action(&action);
    run(&task, &action, tracker, config, Settlement::Settle, monad).await;
}

async fn scenario_one(
    task: &Task,
    tracker: &mut AttemptTracker,
    config: &RainConfig,
    monad: Option<&MonadConfig>,
) {
    header(1, "Clean approval");

    let action = ProposedAction {
        task_id: "scenario-1".to_string(),
        destination: "NYC".to_string(),
        amount: 480.0,
        description: "Delta flight to NYC".to_string(),
    };

    print_task(task);
    print_action(&action);
    run(task, &action, tracker, config, Settlement::Settle, monad).await;
}

/// Scenario 1 again, except a real Claude agent writes the proposed action.
///
/// The gate does not care where the action came from: it runs the same
/// `evaluate` -> `execute_rain_flow` pipeline as every other scenario.
async fn scenario_one_b(
    task: &Task,
    tracker: &mut AttemptTracker,
    config: &RainConfig,
    monad: Option<&MonadConfig>,
) {
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
    run(task, &action, tracker, config, Settlement::Settle, monad).await;
}

async fn scenario_two(
    task: &Task,
    tracker: &mut AttemptTracker,
    config: &RainConfig,
    monad: Option<&MonadConfig>,
) {
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
    run(task, &action, tracker, config, Settlement::Settle, monad).await;
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

async fn scenario_three(
    task: &Task,
    tracker: &mut AttemptTracker,
    config: &RainConfig,
    monad: Option<&MonadConfig>,
) {
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
        run(task, &action, tracker, config, attempt.settlement, monad).await;
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
    run(task, &action, tracker, config, Settlement::Settle, monad).await;
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
    monad: Option<&MonadConfig>,
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

            anchor_decision(monad, task, action, &decision).await;
        }
        Verdict::Denied(decision) => {
            print_decision(&decision);
            println!("  No Rain API calls made — request blocked before reaching payment layer");

            // Blocked and escalated decisions are anchored too. An audit trail
            // that only recorded approvals would answer the least interesting
            // question: the valuable claim is that this system refused, and
            // when.
            anchor_decision(monad, task, action, &decision).await;
        }
    }
    println!();
}

// ------------------------------------------------------------ audit trail ---

/// Writes one decision to the Monad audit log, best effort.
///
/// This runs after the Rain flow has already finished, so nothing it does can
/// change an outcome. A failure is reported and stepped over — the decision
/// stands whether or not the testnet was reachable.
async fn anchor_decision(
    monad: Option<&MonadConfig>,
    task: &Task,
    action: &ProposedAction,
    decision: &Decision,
) {
    let Some(monad) = monad else {
        return;
    };

    let hash = monad_client::hash_decision(task, action, decision);

    match monad_client::log_decision_to_monad(monad, hash, &decision.status).await {
        Ok(tx_hash) => println!("  Decision logged to Monad — tx: {tx_hash}"),
        Err(e) => println!("  (Monad logging skipped: {e})"),
    }
}

/// Reads one decision back off the chain, to close the demo on something this
/// program cannot fake.
///
/// Every other line of output is this program's own account of what it did. The
/// values printed here came from Monad, so they hold up even if you assume the
/// rest of the output is a lie.
async fn verify_one_decision_on_chain(monad: Option<&MonadConfig>) {
    let Some(monad) = monad else {
        return;
    };

    println!("{}", "-".repeat(74));
    println!("  Independently verifying one of the logged decisions directly from");
    println!("  Monad, not from our own program's memory:");
    println!("{}", "-".repeat(74));

    let count = match monad_client::record_count(monad).await {
        Ok(count) => count,
        Err(e) => {
            println!("  (could not reach the audit log: {e})\n");
            return;
        }
    };

    if count == 0 {
        println!("  The audit log is empty — nothing was written this run.\n");
        return;
    }

    // The most recent record: whatever this run wrote last.
    let index = count - 1;

    match monad_client::read_decision_from_monad(monad, index).await {
        Ok((hash, status, timestamp)) => {
            println!("  record #{index} of {count} on chain");
            println!("    decision hash : {hash}");
            println!("    status        : {status}");
            println!("    timestamp     : {timestamp}");
            println!();
            println!("  The hash commits to the task, the proposed action, and the reason.");
            println!("  Anyone holding the original task can recompute it and prove this");
            println!("  decision is the one that was made — and the chain shows nothing else.");
        }
        Err(e) => println!("  (could not read record {index}: {e})"),
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
