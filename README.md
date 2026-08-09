# IntentGuard

**Rain controls whether an agent *can* spend. IntentGuard verifies whether the spend *matches what the user actually asked for*.**

---

## The problem

Rain's Agent Control Layer enforces structural payment limits before an agent transacts — Rain's own materials enumerate the dimensions: merchant category codes, approved merchants and counterparties, transaction amounts, frequency, timing, card expiry, and active card counts. That list is the boundary. Every one of those controls is a property of the *payment*; none of them is a property of the *task*.

So an agent told "get me to NYC for under $500" that books a $450 flight to Miami passes every control Rain has. Right MCC, right amount, first attempt of the day. Rain has no way to know the user said NYC, because the task never reaches the payment layer. Rain's Co-Founder and CTO Charles Yoo-Naut framed the release as being "about scale and accountability" — IntentGuard is the accountability half, sitting one layer above the money and checking the one thing the payment rails structurally cannot see.

## How it works

```
plain English  ──►  Claude parses a structured Task  ──►  Claude (as agent) proposes an Action
                                                                      │
                                                                      ▼
                                                         IntentGuard: Action vs. Task
                                                          │              │           │
                                                    approved         blocked    escalated
                                                          │              │           │
                                                          ▼              └─────┬─────┘
                                            real Rain sandbox calls            │
                                     (scoped card ► authorize ► settle)        │
                                                          │                    │
                                                          ▼                    ▼
                                              ┌───────────────────────────────────┐
                                              │  keccak256 hash ► Monad testnet   │
                                              │  every decision, all three kinds  │
                                              └───────────────────────────────────┘
```

Blocked and escalated actions are anchored on chain too. An audit trail that only recorded approvals would answer the least interesting question; the valuable claim is that the system *refused*, and when.

Only the hash goes on chain — never the task. Destination, budget, purpose, and travel dates are exactly the private context that makes IntentGuard work, and publishing them would defeat the point. Anyone holding the original task can recompute the digest and prove it matches; the chain alone reveals nothing.

### The two shapes

A `Task` is what the user actually said. A `ProposedAction` is what the agent wants to do about it.

```jsonc
// Task — parsed from plain English, never invented
{
  "destination": "NYC",
  "max_budget": 500.0,
  "purpose": "hackathon attendance",
  "start_date": "2026-08-15",   // ISO-8601, the travel window the user stated
  "end_date":   "2026-08-16"
}

// ProposedAction — written by the agent, checked against the task above
{
  "task_id": "scenario-1",
  "destination": "NYC",
  "amount": 480.0,
  "description": "Delta flight to NYC",
  "start_date": "2026-08-15",   // must fall inside the task's window
  "end_date":   "2026-08-16"
}
```

`check_intent` enforces three things: the destination matches, the amount is within budget, and the travel dates fall inside the task's window. Dates are structured fields rather than prose inside `description` for one reason — **a date buried in free text can be displayed but not validated**, so an agent booking days the user never asked to pay for would sail through unchallenged.

The window check is containment, not equality: a shorter trip inside the stated days is still the trip that was asked for, while a day outside it is travel the user never agreed to. The comparison is plain string comparison, which is chronologically correct precisely because the dates are zero-padded `YYYY-MM-DD` — a shape the parsing layer enforces before any value reaches the gate.

## What is genuinely real here

**Real Claude API calls — two distinct ones.** `claude-opus-5` via the Messages API, 8-second timeout, thinking disabled. One call extracts a `Task` from plain English (`parse_task_from_prompt`); a separate call, with a different system prompt, plays the booking agent and proposes an action (`agent_propose_action`). The agent does not get to pick its own `task_id` — the attempt tracker keys non-convergence on it, so generated text choosing it would let the model dodge escalation.

**Real Rain sandbox transactions.** `api-dev.raincards.xyz/v1`, RSA-encrypted session id, then `POST /issuing/users/{userId}/cards/scoped` → `POST /simulate/transactions/authorize` → `POST /simulate/transactions/{id}/settle`, MCC 4511. Real IDs come back and are printed, e.g. card `d0474d5f-3077-4c9d-aaf6-58da9566117e`, transaction `64b6cf6c-92fa-481a-aa77-dcf46e84cc38`.

**Real Monad testnet contract.** `IntentGuardAuditLog` at [`0xf70b88E30F844B400EA8478A73f181854Bd16cEa`](https://testnet.monadexplorer.com/address/0xf70b88E30F844B400EA8478A73f181854Bd16cEa), chain id 10143, written from `0xd4b0FA949C95627eC358d9786FCCD9F40dFEf51E`. A full run writes seven records. Read back live off chain, not from program memory:

```
record #8  0x7435b6fc4157642cfda6069fbb2628ff59e734c13c538b26ccaeefa4d5777313  approved
record #9  0xc2190dbdf877a752a7550a979b3d99bfba18134a5cb36a8dc89af6cf3b7b48f4  blocked
record #12 0x702756d00e48eb3b0efda4721ccc81ea36312012d7c13e9282efcf206455d5e4  escalated
```

Verify any of it yourself, with no part of this repo involved:

```bash
cast call 0xf70b88E30F844B400EA8478A73f181854Bd16cEa 'recordCount()(uint256)' --rpc-url <monad-testnet-rpc>
cast call 0xf70b88E30F844B400EA8478A73f181854Bd16cEa 'getRecord(uint256)(bytes32,string,uint256)' 9 --rpc-url <monad-testnet-rpc>
```

**A real compile-time guarantee.** This is the part that is not a runtime check at all. `ApprovedAction` wraps a `&ProposedAction` in a field private to `intent_check`, so the only way to construct one is for `evaluate` to return `Verdict::Approved`:

```rust
pub struct ApprovedAction<'a>(&'a ProposedAction);   // field is private to this module

pub async fn execute_rain_flow(
    approved: &ApprovedAction<'_>,   // not &ProposedAction
    config: &RainConfig,
    mcc: &str,
    settlement: Settlement,
) -> Result<RainResult, Error>
```

Calling Rain on a blocked or escalated action is not a bug that code review has to catch — there is no value of the right type to pass, so it does not compile. The denied branch in `main.rs` has no token and therefore no reachable path to the payment layer.

## The five scenarios

| | Scenario | What it shows |
|---|---|---|
| **0** | Fully live | Natural language → task → agent → decision, nothing scripted but the sentence. Both Claude calls are live and the outcome is not predetermined — this scenario can legitimately come out blocked. That is not a failure of the demo, it *is* the demo. |
| **1** | Clean approval | Deterministic baseline: hardcoded NYC booking at $480 against a $500 budget, on the task's dates. Approved, card issued, charge settled. |
| **1B** | Live agent proposal | Same task as scenario 1, but a real Claude agent writes the action. The gate does not care where the action came from — identical `evaluate` → `execute_rain_flow` path. Falls back to the known-good booking and says so if the call fails. |
| **2** | Intent mismatch | $450 flight to Miami, on the right dates and under budget. Rain's Agent Control Layer would allow this — airline MCC, sane amount, first attempt. Only the task context reveals it is the wrong city, and the city is the *only* thing wrong with it: a regression test corrects the destination alone and asserts the same booking then passes, so this scenario stays evidence about one dimension. **Blocked before any Rain API call is made.** |
| **3** | Non-convergence | The agent keeps trying and keeps failing. Two attempts authorize but are deliberately **never settled** — the card was valid, the booking still did not happen, no money moved. On the third, IntentGuard escalates to human review; that attempt never reaches Rain at all. Each individual action is valid; the *pattern* is the problem. |

## The no-fabrication guard

`parse_task_from_prompt` has **no fallback**, on purpose. Inventing a destination, a budget, or a travel date the user never gave would fabricate the exact intent this project exists to check. A vague request is an error the caller reports, not a gap to fill in — and the guard is enforced twice: the model is instructed to return `{"error": ...}` when a field is missing, *and* the parser independently rejects an empty destination, a non-finite or non-positive budget, an empty purpose, any date that is not a full `YYYY-MM-DD`, and a window that ends before it starts, in case a model answers in the right shape with nothing in it.

Dates are held to that standard strictly: **a month and a day with no year is incomplete.** "August 15th" does not say which August, and picking the nearest one is a guess about what the user meant — the same class of guess the destination and budget rules already forbid. The example request therefore states the year outright, and an interactive request has to carry one too.

Tested live with a deliberately ambiguous prompt:

```
> book me something nice

  Parsing task from natural language...
  COULD NOT PARSE A VALID TASK: the request is incomplete — destination and budget are both missing
  In production this would prompt the user to clarify.
```

No task invented, no agent invoked, no Rain call, no money path opened.

## Architecture

| File | Responsibility |
|---|---|
| `src/types.rs` | `Task`, `ProposedAction`, `Decision` — the three shapes everything else moves around. Both travel-bearing types carry `start_date`/`end_date` as ISO-8601 strings. |
| `src/intent_check.rs` | The gate: `check_intent` (destination, budget, travel window), the `AttemptTracker` for non-convergence, and `evaluate`, the only function that can mint an `ApprovedAction`. |
| `src/agent.rs` | Both Claude calls — natural language → `Task`, and `Task` → proposed booking — plus the parsing that refuses to guess. |
| `src/rain_client.rs` | Rain sandbox lifecycle: RSA session id, scoped card issuance, authorize, settle. Takes `&ApprovedAction`. |
| `src/monad_client.rs` | Decision hashing (keccak256 over task + action + status + reason) and the Monad reads/writes, over one shared provider. |
| `src/main.rs` | The five scenarios, the single `run` gate, and the closing read-back from chain. |
| `contracts/src/IntentGuardAuditLog.sol` | 46-line append-only log: `logDecision`, `getRecord`, `recordCount`. |

## Running it

Tests are fully offline — verified by running them in an unshared network namespace, where all 57 still pass:

```bash
cargo test          # 57 passed; 0 failed — finishes in 0.02s, zero network calls
```

(19 in `intent_check`, 20 in `agent`, 6 in `monad_client`, 6 in `rain_client`, 6 integration-level in `main`.)

The full demo needs a `.env`:

```
RAIN_API_KEY=
RAIN_USER_ID=
RAIN_TEAM_ID=
RAIN_CONTRACT_ID=
ANTHROPIC_API_KEY=
MONAD_RPC_URL=
MONAD_PRIVATE_KEY=
MONAD_CONTRACT_ADDRESS=
```

```bash
cargo run                              # all five scenarios
INTENTGUARD_INTERACTIVE=1 cargo run    # type your own request into scenario 0
```

The Monad layer is optional by construction. A missing key or an unreachable testnet degrades the demo to exactly what it was before Monad existed, rather than stopping a decision from being made — anchoring runs *after* the Rain flow, so nothing it does can change an outcome.

## What this is not

Being specific about the edges, since the parts above are specific about the claims:

- **Not a production system.** It is a demo that makes real API calls, which is a different thing.
- **No UI.** Terminal output only.
- **No connection pooling, rate limiting, or retries** on the Rain or Monad clients. A transient failure is reported and stepped over, not recovered from.
- **No persistent task registry.** The task and the action travel together per request. A real deployment would store the task at creation and have the gate look it up, so the caller cannot hand over a task that flatters the action.
- **MCC is a fixed default** (`4511`, airlines), not derived from the proposed booking.
- **Nonce caching has a sharp edge.** One shared provider fixes duplicate-nonce rejections across a run, but alloy's cached nonce does not roll back on a failed send, so a rejected write would leave later writes in that run with a nonce gap.
- **Writes are fire-and-forget.** `log_decision_to_monad` returns once the transaction is accepted, not once it is mined — so "logged to Monad" means submitted. (Every record in this README was confirmed mined by reading it back independently.)

---

Built for the **Raingentic Commerce Hackathon**, August 2026.

Rust 2024 edition · `alloy` 2.1.1 · ~2,100 lines including tests and the contract.
