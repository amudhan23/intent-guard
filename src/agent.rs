//! An optional LLM step: ask Claude to propose the booking instead of
//! hardcoding it.
//!
//! Everything here is best-effort. The demo must never fail because a network
//! call was slow or a model wrote prose around its JSON, so the entry point
//! callers use — [`propose_or_fallback`] — always returns a usable action and
//! reports where it came from.

use std::time::Duration;

use serde_json::Value;

use crate::types::{ProposedAction, Task};

const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MODEL: &str = "claude-opus-5";

/// Bounds the whole request. A slow answer becomes a fallback, not a hang.
const TIMEOUT: Duration = Duration::from_secs(8);

/// The task id this scenario runs under.
///
/// The model does not get to choose it: `AttemptTracker` keys non-convergence
/// on the task id, so letting generated text decide it would let the model
/// dodge — or trip — the escalation logic.
pub const AGENT_TASK_ID: &str = "scenario-1b";

// ---------------------------------------------------------------- errors ----

#[derive(Debug)]
pub enum Error {
    MissingEnv(&'static str),
    Http(reqwest::Error),
    Api {
        status: reqwest::StatusCode,
        body: String,
    },
    /// The call succeeded but the text was not a `ProposedAction`.
    Parse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::MissingEnv(var) => write!(f, "missing environment variable {var}"),
            Error::Http(e) if e.is_timeout() => {
                write!(f, "claude did not answer within {}s", TIMEOUT.as_secs())
            }
            Error::Http(e) => write!(f, "http transport error: {e}"),
            Error::Api { status, body } => write!(f, "anthropic api returned {status}: {body}"),
            Error::Parse(detail) => write!(f, "could not read a proposed action: {detail}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

// ---------------------------------------------------------------- prompt ----

fn system_prompt() -> String {
    "You are a travel booking agent. You are given a task and you propose exactly one \
     specific flight booking that satisfies it.\n\n\
     Reply with a single JSON object and nothing else — no prose, no markdown fences, \
     no explanation. Your reply is parsed directly by a program.\n\n\
     Do not include internal or system XML tags in your response."
        .to_string()
}

fn user_prompt(task: &Task) -> String {
    format!(
        "TASK\n  destination : {}\n  max_budget  : {:.2} USD\n  purpose     : {}\n\n\
         Propose one flight booking for this task as JSON with exactly these keys:\n\
         {{\n  \"task_id\": \"{}\",\n  \"destination\": \"{}\",\n  \
         \"amount\": <number, USD, at most {:.2}>,\n  \
         \"description\": \"<airline and route, e.g. 'Delta flight to NYC'>\"\n}}",
        task.destination,
        task.max_budget,
        task.purpose,
        AGENT_TASK_ID,
        task.destination,
        task.max_budget,
    )
}

// ------------------------------------------------------------- the call -----

/// Asks Claude for a booking. Fails loudly; [`propose_or_fallback`] is what the
/// demo calls.
pub async fn agent_propose_action(task: &Task) -> Result<ProposedAction, Error> {
    let api_key = match std::env::var("ANTHROPIC_API_KEY") {
        Ok(key) if !key.trim().is_empty() => key,
        _ => return Err(Error::MissingEnv("ANTHROPIC_API_KEY")),
    };

    let response = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()?
        .post(MESSAGES_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .json(&serde_json::json!({
            "model": MODEL,
            "max_tokens": 512,
            // Thinking is on by default on this model. A booking proposal does
            // not need it, and the demo does not have seconds to spare.
            "thinking": { "type": "disabled" },
            "output_config": { "effort": "low" },
            "system": system_prompt(),
            "messages": [{ "role": "user", "content": user_prompt(task) }],
        }))
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        return Err(Error::Api { status, body });
    }

    let action = parse_action(&body)?;
    Ok(action)
}

/// Pulls the first text block out of a Messages API response.
fn first_text_block(body: &str) -> Result<String, Error> {
    let parsed: Value = serde_json::from_str(body)
        .map_err(|e| Error::Parse(format!("response was not json ({e}): {body}")))?;

    // A refusal is a 200 with an empty content array — check before indexing.
    if parsed.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        return Err(Error::Parse("the model declined the request".to_string()));
    }

    parsed
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        })
        .and_then(|block| block.get("text"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::Parse(format!("no text block in {body}")))
}

/// Reads a `ProposedAction` out of a raw API response body.
fn parse_action(body: &str) -> Result<ProposedAction, Error> {
    let text = first_text_block(body)?;
    action_from_text(&text)
}

/// Tolerates a model that wraps its JSON in prose or a markdown fence by
/// taking the outermost braced span.
fn action_from_text(text: &str) -> Result<ProposedAction, Error> {
    let start = text
        .find('{')
        .ok_or_else(|| Error::Parse(format!("no json object in {text:?}")))?;
    let end = text
        .rfind('}')
        .ok_or_else(|| Error::Parse(format!("no json object in {text:?}")))?;
    if end < start {
        return Err(Error::Parse(format!("no json object in {text:?}")));
    }

    let mut action: ProposedAction = serde_json::from_str(&text[start..=end])
        .map_err(|e| Error::Parse(format!("{e} in {:?}", &text[start..=end])))?;

    // The tracker keys on this, so it is ours to set, not the model's.
    action.task_id = AGENT_TASK_ID.to_string();
    Ok(action)
}

// ------------------------------------------------------------- fallback -----

/// Where a proposed action came from. Printed so the demo never implies an
/// agent decided something it did not.
pub enum Origin {
    Claude,
    /// Carries why the live call was not used.
    Fallback(String),
}

/// The hardcoded scenario-1 booking, reused when the live call cannot be.
fn fallback_action() -> ProposedAction {
    ProposedAction {
        task_id: AGENT_TASK_ID.to_string(),
        destination: "NYC".to_string(),
        amount: 480.0,
        description: "Delta flight to NYC".to_string(),
    }
}

/// Always returns an action. Never panics, never hangs past [`TIMEOUT`].
pub async fn propose_or_fallback(task: &Task) -> (ProposedAction, Origin) {
    match agent_propose_action(task).await {
        Ok(action) => (action, Origin::Claude),
        Err(e) => (fallback_action(), Origin::Fallback(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No test in this module makes a network call — `cargo test` stays offline.
    fn body(text: &str) -> String {
        serde_json::json!({
            "content": [{ "type": "text", "text": text }],
            "stop_reason": "end_turn",
        })
        .to_string()
    }

    #[test]
    fn parses_a_clean_json_reply() {
        let action = parse_action(&body(
            r#"{"task_id":"x","destination":"NYC","amount":465.0,"description":"JetBlue to NYC"}"#,
        ))
        .expect("parse");
        assert_eq!(action.destination, "NYC");
        assert_eq!(action.amount, 465.0);
        assert_eq!(action.description, "JetBlue to NYC");
    }

    #[test]
    fn overrides_whatever_task_id_the_model_chose() {
        let action = parse_action(&body(
            r#"{"task_id":"whatever-i-want","destination":"NYC","amount":465.0,"description":"f"}"#,
        ))
        .expect("parse");
        assert_eq!(action.task_id, AGENT_TASK_ID);
    }

    #[test]
    fn tolerates_prose_and_fences_around_the_json() {
        let action = action_from_text(
            "Sure! Here you go:\n```json\n{\"task_id\":\"x\",\"destination\":\"NYC\",\
             \"amount\":470.0,\"description\":\"United to NYC\"}\n```\nHope that helps.",
        )
        .expect("parse");
        assert_eq!(action.amount, 470.0);
    }

    #[test]
    fn rejects_a_reply_with_no_json() {
        assert!(matches!(
            action_from_text("I'd rather not."),
            Err(Error::Parse(_))
        ));
    }

    #[test]
    fn rejects_json_missing_a_field() {
        assert!(matches!(
            action_from_text(r#"{"destination":"NYC","amount":470.0}"#),
            Err(Error::Parse(_))
        ));
    }

    #[test]
    fn reports_a_refusal_rather_than_indexing_empty_content() {
        let refused = serde_json::json!({
            "content": [],
            "stop_reason": "refusal",
        })
        .to_string();
        assert!(matches!(parse_action(&refused), Err(Error::Parse(_))));
    }

    #[test]
    fn fallback_matches_the_hardcoded_scenario_one_booking() {
        let action = fallback_action();
        assert_eq!(action.destination, "NYC");
        assert_eq!(action.amount, 480.0);
        assert_eq!(action.description, "Delta flight to NYC");
    }
}
