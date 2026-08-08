# IntentGuard — Rain Integration Build Spec (for Claude Code)

## Context

This is an existing Rust project (`req-guard`, built on Tokio + Hyper) that validates HTTP requests against rules before forwarding them. It has working:
- `intent_check.rs` — `check_intent(task: &Task, action: &ProposedAction) -> Decision` and `AttemptTracker` (non-convergence counter), both unit-tested
- `types.rs` — `Task`, `ProposedAction`, `Decision` structs
- A Hyper server with routing, body buffering, and forwarding logic already working (`req-guard`)

## Goal

Wire in real calls to Rain's sandbox API so that when `check_intent` approves an action, the code actually issues a scoped card and runs it through the authorize/settle lifecycle — instead of just returning a canned response.

## Rain sandbox details

Base URL: `https://api-dev.raincards.xyz/v1`

Credentials are provided via environment variables (already set by the user, DO NOT hardcode or print them):
- `RAIN_API_KEY`
- `RAIN_USER_ID`
- `RAIN_TEAM_ID`
- `RAIN_CONTRACT_ID`

## Required flow, in order

### 1. Fund collateral (call once at startup, not per-request)
```
POST /simulate/collateral/fund
Headers: Api-Key: {RAIN_API_KEY}
Body: { "contractId": "{RAIN_CONTRACT_ID}", "currency": "rusd", "amount": 100000 }
```

### 2. Generate a session ID (needed before issuing a card)

Rain requires an RSA-OAEP-encrypted session ID, generated client-side, using their published sandbox public key (below). This session ID goes in a `sessionid` header when issuing a scoped card.

**Sandbox public key (RSA, PEM format):**
```
-----BEGIN PUBLIC KEY-----
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQCAP192809jZyaw62g/eTzJ3P9H
+RmT88sXUYjQ0K8Bx+rJ83f22+9isKx+lo5UuV8tvOlKwvdDS/pVbzpG7D7NO45c
0zkLOXwDHZkou8fuj8xhDO5Tq3GzcrabNLRLVz3dkx0znfzGOhnY4lkOMIdKxlQb
LuVM/dGDC9UpulF+UwIDAQAB
-----END PUBLIC KEY-----
```

**Algorithm (reference implementation, Node.js, needs porting to Rust):**
```js
import crypto from "crypto";

async function generateSessionId(pem, secret) {
  const secretKey = secret ?? crypto.randomUUID().replace(/-/g, ""); // 32-char hex
  const secretKeyBase64 = Buffer.from(secretKey, "hex").toString("base64");
  const secretKeyBase64Buffer = Buffer.from(secretKeyBase64, "utf-8");
  const encrypted = crypto.publicEncrypt(
    {
      key: pem,
      padding: crypto.constants.RSA_PKCS1_OAEP_PADDING,
      oaepHash: 'sha1'
    },
    secretKeyBase64Buffer,
  );
  return { secretKey, sessionId: encrypted.toString("base64") };
}
```

**Rust port requirements:**
- Use the `rsa` crate for RSA-OAEP encryption with SHA-1 as the hash (matches `oaepHash: 'sha1'` above)
- Generate a random 32-character hex string for the secret (use `rand` crate)
- Base64-encode the hex secret string itself first (note: encrypting the *base64 representation* of the hex string, not the raw bytes — match the JS reference exactly)
- RSA-OAEP-encrypt that base64 string using the public key above
- Base64-encode the encrypted result — this is the `sessionId` to send as a header

**IMPORTANT: we do NOT need to decrypt the response's encryptedPan/encryptedCvc fields.** We only need the `id` field from the scoped-card response (the `cardId`), which is used in subsequent calls. Skip implementing the AES-GCM decryption step entirely — it's not needed for this project's flow, since we never use the raw card number anywhere.

### 3. Issue a scoped card (per approved action)
```
POST /issuing/users/{RAIN_USER_ID}/cards/scoped
Headers:
  Api-Key: {RAIN_API_KEY}
  sessionid: {generated in step 2}
  content-type: application/json
Body: { "amountInUSDCents": <action.amount * 100 as integer> }
```
Response includes `id` — this is the `cardId`, save it. Ignore `encryptedPan`/`encryptedCvc`.

### 4. Authorize the transaction
```
POST /simulate/transactions/authorize
Headers: Api-Key: {RAIN_API_KEY}
Body: {
  "cardId": "<from step 3>",
  "amount": <action.amount * 100 as integer>,
  "currency": "usd",
  "merchantName": "<action.description or a derived merchant name>",
  "merchantCategoryCode": "<appropriate MCC, e.g. airline>"
}
```
Response includes a transaction `id`. Save it.

### 5. Settle the transaction
```
POST /simulate/transactions/{id}/settle
Headers: Api-Key: {RAIN_API_KEY}
```
(where `{id}` is the transaction id from step 4)

### 6. Read back the transaction (for the demo output)
```
GET /issuing/transactions?limit=20
Headers: Api-Key: {RAIN_API_KEY}
```

## Where this plugs into existing code

In the request handler (wherever `check_intent` is currently called), after getting a `Decision`:

```rust
let decision = check_intent(&task, &action);

match decision.status.as_str() {
    "approved" => {
        // NEW: call the Rain chain (steps 3-6 above)
        match execute_rain_flow(&action, &rain_config).await {
            Ok(rain_result) => {
                // return decision + rain_result (cardId, transactionId, status) to the caller
            }
            Err(e) => {
                // log the error, return decision with a note that Rain execution failed
            }
        }
    }
    "blocked" | "escalated" => {
        // existing behavior — do NOT call Rain at all
    }
    _ => unreachable!(),
}
```

**Critical design point: blocked and escalated decisions must NEVER trigger any Rain API call.** This is the actual point of the project — bad or stuck transactions are stopped before reaching Rain. Make sure the code structure makes this impossible to get wrong (e.g., the Rain-calling function should only ever be reachable from the "approved" branch).

## New module to create

`src/rain_client.rs` containing:
- `struct RainConfig { api_key: String, user_id: String, contract_id: String }` — loaded from env vars at startup
- `fn generate_session_id() -> Result<(String, String), Error>` — returns (secret_key, session_id), implementing the algorithm above
- `async fn fund_collateral(config: &RainConfig) -> Result<(), Error>` — step 1, call once at startup
- `async fn issue_scoped_card(config: &RainConfig, amount_cents: u64, session_id: &str) -> Result<String, Error>` — step 3, returns cardId
- `async fn authorize_transaction(config: &RainConfig, card_id: &str, amount_cents: u64, merchant_name: &str, mcc: &str) -> Result<String, Error>` — step 4, returns transaction id
- `async fn settle_transaction(config: &RainConfig, transaction_id: &str) -> Result<(), Error>` — step 5
- `async fn execute_rain_flow(action: &ProposedAction, config: &RainConfig) -> Result<RainResult, Error>` — orchestrates steps 3-5 in sequence, returns a struct with cardId, transactionId, status

Use `reqwest` for the HTTP client (simpler than raw `hyper::client` for this — sequential, one-off calls, not a proxy's concurrent connection handling). Add `reqwest = { version = "0.12", features = ["json"] }` to Cargo.toml if not present. Also add `rsa`, `rand`, `base64`, `hex` crates for the session ID generation.

## Testing expectations

- Unit test `generate_session_id` produces a valid base64 string of the expected length, without needing to hit the real API
- Do NOT write integration tests that call the real Rain sandbox automatically (avoid burning through demo data / rate limits during CI or repeated test runs) — manual testing against the sandbox via the actual demo scenarios is sufficient
- Existing `check_intent`/`AttemptTracker` tests should remain unchanged and passing

## Explicitly out of scope — do not build

- AES-GCM decryption of PAN/CVC (not needed, see note above)
- Connection pooling, retries, rate limiting for the Rain client (hackathon demo, not production)
- Any UI beyond clean terminal/JSON output
- Monad integration (separate, optional stretch goal, not part of this task)

## Success criteria

Running the existing 3 demo scenarios (clean approval, intent mismatch, non-convergence) against this updated code should result in:
1. Clean approval scenario: real cardId, real transaction id, real settled status, all visible in output
2. Intent mismatch scenario: blocked, no Rain calls made at all (verify no network calls happen)
3. Non-convergence scenario: escalated after 3rd attempt, no Rain calls made at all
