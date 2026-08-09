Add natural-language task parsing to IntentGuard. This lets a real user type a plain-English request, which gets parsed into the structured Task, then passed through the existing agent_propose_action -> check_intent -> Rain flow, completely unscripted and live.

## Context

The project already has: Task/ProposedAction/Decision types, check_intent validation logic, an AttemptTracker for non-convergence, a working Rain sandbox client, and a working agent_propose_action(&Task) -> ProposedAction function in src/agent.rs that calls Claude to propose a booking given a task. Scenarios 1, 1B, 2, and 3 in main.rs are complete, tested, and must NOT be modified or removed — they remain the guaranteed-reliable fallback demo.

## Goal

Add a new function that takes a raw natural-language string and parses it into a Task struct via Claude, then a new "Scenario 0" entry point in main.rs that chains: natural language -> parsed Task -> agent_propose_action -> check_intent -> Rain flow (if approved). This is a genuinely live, unscripted path — the parsed task and the proposed action are not hardcoded, so this scenario may organically produce a mismatch or an over-budget proposal, and that's expected and fine — it should be handled by the existing check_intent logic exactly the same as scenario 2.

## Step 1: Add parse_task_from_prompt to src/agent.rs

```
async fn parse_task_from_prompt(user_input: &str) -> Result<Task, AgentError>
```

- Same HTTP client pattern already used in agent_propose_action (reqwest, same 8-second timeout, same claude-opus-5 model with thinking disabled and effort low)
- The prompt to Claude should ask it to extract from the user's natural-language request: destination (city name), max_budget (a number, in USD), and purpose (a short string describing why). Explicitly instruct it to respond with ONLY valid JSON matching the Task schema, nothing else.
- Parse the response the same way agent_propose_action does — same robustness to markdown code fences, prose wrapping, missing fields (reuse whatever JSON-extraction helper already exists from agent_propose_action rather than duplicating logic)
- If the user's input is too vague to extract a destination or budget (e.g. missing entirely), this should return an error rather than guessing — do not fabricate a destination or budget that wasn't stated. This is important: a real user might type an incomplete request, and IntentGuard's job starts with getting a well-formed task, not inventing one.
- Unlike agent_propose_action, there is NO fallback-to-hardcoded-value here, because there's no sensible fallback for "what did the user actually ask for" — if parsing fails, return the error clearly so the caller can tell the user to rephrase.
- Add offline unit tests (no network) covering: clean JSON parses correctly, JSON wrapped in prose/markdown parses correctly, missing required field returns an error, completely non-JSON response returns an error. Follow the same test patterns already used for agent_propose_action's parsing tests.

## Step 2: Add Scenario 0 to main.rs

Add this as the FIRST scenario printed (before Scenario 1), clearly labeled as the live, unscripted one:

```
==========================================================================
  SCENARIO 0 — Fully live: natural language -> task -> agent -> decision
==========================================================================
```

Use this hardcoded example prompt for the default demo run (since we need something reliable to type/show, but the parsing itself is genuinely live, not hardcoded):

"I need to get to New York City for a hackathon happening August 15th and 16th, 2026. My budget is $500. This is for me, traveling alone."

Print each step clearly as it happens:
1. "User request: <the raw prompt>"
2. "Parsing task from natural language..."
3. Print the parsed Task (destination, max_budget, purpose) — make clear this came from the LLM, not from code
4. "Asking a real Claude agent to propose a booking for this task..." (reuse exact wording from scenario 1B)
5. Print the proposed action
6. Run through check_intent exactly as the other scenarios do
7. Print the decision and, if approved, the Rain flow result — if blocked or escalated, print that clearly, same format as scenarios 2 and 3

If parse_task_from_prompt returns an error, print a clear message ("Could not parse a valid task from the request — in production this would prompt the user to clarify") and skip to Scenario 1, do not crash the whole program.

## Step 3: Optional interactive mode (only if time allows after Step 1 and 2 are solid)

Add a command-line flag or environment variable (e.g. INTENTGUARD_INTERACTIVE=1) that, if set, prompts the user to type their own natural-language request via stdin instead of using the hardcoded example string. This lets a judge type a live request themselves during the demo if desired. Keep the hardcoded-example path as the default when the flag isn't set, since that's more reliable for a scripted demo run.

## Constraints

- Do not modify check_intent, AttemptTracker, the Rain client, or scenarios 1, 1B, 2, 3 in any way
- Do not add any test that makes a real network call automatically during cargo test
- Same 8-second timeout pattern as the existing agent code, so a slow response doesn't hang the demo
- If parse_task_from_prompt's Claude call itself fails entirely (network error, API error, timeout) rather than returning bad JSON, handle this the same way as a parse failure — print a clear message, skip gracefully, do not crash

## Final check

Run cargo test (all existing tests plus new ones should pass, no network needed) and cargo run (this makes real calls to both Claude and, if Scenario 0 is approved, Rain). Show me the full output of Scenario 0 specifically, including exactly what Task and ProposedAction got generated from the example prompt.
