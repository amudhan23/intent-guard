mod intent_check;
mod rain_client;
mod types;

use intent_check::{AttemptTracker, Verdict, evaluate};
use rain_client::{DEFAULT_MCC, RainConfig};
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
    run(task, &action, tracker, config).await;
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
    run(task, &action, tracker, config).await;
}

async fn scenario_three(task: &Task, tracker: &mut AttemptTracker, config: &RainConfig) {
    header(3, "Non-convergence");

    print_task(task);
    println!("  The agent retries the same task three times. Each action is");
    println!("  individually valid; the pattern is the problem.\n");

    let attempts = [
        ("JetBlue flight to NYC", 460.0),
        ("United flight to NYC", 470.0),
        ("American flight to NYC", 455.0),
    ];

    for (index, (description, amount)) in attempts.iter().enumerate() {
        let action = ProposedAction {
            task_id: "scenario-3".to_string(),
            destination: "NYC".to_string(),
            amount: *amount,
            description: description.to_string(),
        };

        println!("  --- attempt {} of {} ---", index + 1, attempts.len());
        print_action(&action);
        run(task, &action, tracker, config).await;
    }
}

// --------------------------------------------------------------- plumbing ---

/// The single gate. `execute_rain_flow` takes the approval token that only
/// `Verdict::Approved` carries, so the denied arm below has no way to spend.
async fn run(
    task: &Task,
    action: &ProposedAction,
    tracker: &mut AttemptTracker,
    config: &RainConfig,
) {
    match evaluate(task, action, tracker, STUCK_THRESHOLD) {
        Verdict::Approved { token, decision } => {
            print_decision(&decision);
            println!("  Proceeding to Rain...");

            match rain_client::execute_rain_flow(&token, config, DEFAULT_MCC).await {
                Ok(result) => {
                    println!("  RAIN CARD ISSUED AND CHARGE SETTLED");
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
