This is a two-part update to the existing IntentGuard project. Do Part 1 first, fully, and confirm it works before starting Part 2.

## Context

IntentGuard already exists and works: it validates proposed agent actions against a task's constraints before calling Rain's sandbox API to issue a scoped card, authorize, and settle a transaction. There are three demo scenarios in main.rs: clean approval, intent mismatch (blocked), and non-convergence (escalated after 3 attempts).

## Part 1: Fix the non-convergence scenario (do this first, required)

### The problem

Currently, scenario 3 (non-convergence) has the agent propose three DIFFERENT flights for the same task, and the first two attempts are each independently approved AND settled as real, separate transactions before the third attempt escalates. This is wrong — it results in two real bookings for a single task, which is an actual double-booking bug, not just a demo simplification. The intended meaning of "non-convergence" is that the agent keeps trying and FAILING to complete the task, not that it succeeds multiple times.

### The fix

Restructure scenario 3 so that attempts 1 and 2 represent realistic booking failures — the transaction is proposed and IntentGuard approves it (destination and budget are fine), but the simulated purchase itself fails at the Rain authorization step (e.g., simulate a declined/failed authorization — check what Rain's sandbox `/simulate/transactions/authorize` endpoint returns for a failure case, or if the sandbox doesn't easily support simulating a decline, simply do NOT call `settle_transaction` for attempts 1 and 2 — issue the card and authorize, but explicitly do not settle, and print a message like "authorization succeeded but settlement was deliberately skipped — flight unavailable, agent must retry" to make clear no real charge completed).

Only attempt 3 should trigger escalation (via the existing AttemptTracker/is_stuck logic), and escalation should still prevent any Rain call for that specific attempt, exactly as before.

Update the printed scenario narrative to make this clear: attempt 1 fails ("flight sold out, retrying"), attempt 2 fails ("no seats available, retrying"), attempt 3 is escalated by IntentGuard before a third attempt is even made ("pattern detected: 2 failed attempts on this task, escalating to human review rather than trying again").

Keep all existing tests passing. Add a test if needed confirming settle_transaction is not called (or a card is not fully completed) for the first two attempts in this scenario, if that's testable without hitting the network — otherwise this is fine to verify manually via cargo run output.

Run cargo test and cargo run after this change and confirm all three scenarios still behave correctly, especially confirming no double-settlement happens in scenario 3.

## Part 2: Optional agent layer for Scenario 1 only (only do this after Part 1 is confirmed working, and only if there's time)

### Goal

Add a genuine LLM-driven agent step for Scenario 1 (the clean approval case) ONLY. Scenarios 2 and 3 stay exactly as they are — deterministic, hardcoded, reliable — because they need to be guaranteed to work correctly during a live demo. Scenario 1 gets upgraded to actually call an LLM to generate the proposed action from the natural-language task, making it a more honest "an agent really decided this" moment.

### Implementation

Add a new function `async fn agent_propose_action(task: &Task) -> Result<ProposedAction, Error>` in a new module `src/agent.rs`.

This function should:
1. Take the Task struct
2. Construct a prompt to Claude's API (use the `anthropic-sdk` Rust crate if it exists and is straightforward to add, otherwise use `reqwest` directly against the Anthropic Messages API at `https://api.anthropic.com/v1/messages`)
3. The prompt should ask Claude to act as a booking agent: given the task (destination, budget, purpose), propose a specific flight booking as JSON matching the ProposedAction schema (task_id, destination, amount, description). Explicitly instruct it to respond with ONLY valid JSON matching this schema, nothing else, since the response needs to be parsed directly.
4. Parse the response into a ProposedAction. If parsing fails for any reason, fall back to the existing hardcoded scenario 1 action (destination NYC, amount 480.0, description "Delta flight to NYC") rather than crashing or hanging the demo. This fallback is important — log clearly when the fallback is used, but never let this cause the demo to fail visibly.
5. Requires an ANTHROPIC_API_KEY environment variable (added to .env alongside the existing Rain variables), loaded the same way via dotenvy.

### In main.rs

Add a scenario 1B (a second version of scenario 1) that calls agent_propose_action(&task) instead of using the hardcoded ProposedAction, then runs it through the exact same check_intent -> execute_rain_flow pipeline as before. Print output showing: "Asking a real Claude agent to propose a booking for this task..." then show what the agent actually generated, then proceed through the normal decision/Rain flow.

Keep the original hardcoded scenario 1 as-is (for the guaranteed fallback / reliable version). Scenario 1B is an addition, not a replacement.

### Constraints

- Set a reasonable timeout on the Claude API call (5-10 seconds) so a slow response doesn't hang the demo — if it times out, fall back to the hardcoded action, same as a parse failure.
- Do not add any test that makes a real network call to the Anthropic API automatically during cargo test — this should only run during cargo run, manually.
- Keep this entire addition isolated in src/agent.rs so it's easy to skip or remove if it's not working well close to demo time.

### Final check

After Part 2, run cargo test (should still pass, no network needed) and cargo run (this will now make a real call to Claude's API in addition to Rain's, for scenario 1B specifically). Show me the full output including what the agent actually proposed.

## Priority note

If time runs short, Part 1 alone is a complete, correct, demo-ready state. Part 2 is a genuine enhancement but not required — do not sacrifice reliability of the existing three scenarios to rush Part 2. If Part 2 introduces any instability, flag it clearly rather than leaving broken code in place.
