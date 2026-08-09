//! The LLM steps: read a task out of plain English, and propose a booking for
//! a task. Both go through Claude; neither is trusted blindly.
//!
//! The two entry points fail differently, on purpose:
//!
//! - [`propose_or_fallback`] always returns a usable action and reports where
//!   it came from, so a slow or malformed answer degrades the demo instead of
//!   ending it.
//! - [`parse_task_from_prompt`] has no fallback at all. Guessing at a
//!   destination or a budget the user never said would invent the very intent
//!   this project exists to check.

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
    /// The call succeeded but the text was not the shape we asked for.
    Parse(String),
    /// The call succeeded and the model answered honestly that the user's
    /// request does not contain enough to build a task.
    Incomplete(String),
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
            Error::Parse(detail) => write!(f, "could not read the model's reply: {detail}"),
            Error::Incomplete(missing) => write!(f, "the request is incomplete — {missing}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

// ------------------------------------------------------------- transport ----

/// One Messages API round trip, returning the model's text.
///
/// Both the task parser and the booking agent go through here, so the timeout,
/// model, and thinking settings are stated once.
async fn call_claude(system: &str, user: &str) -> Result<String, Error> {
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
            // Thinking is on by default on this model. Neither of these calls
            // needs it, and the demo does not have seconds to spare.
            "thinking": { "type": "disabled" },
            "output_config": { "effort": "low" },
            "system": system,
            "messages": [{ "role": "user", "content": user }],
        }))
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        return Err(Error::Api { status, body });
    }

    first_text_block(&body)
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
        "TASK\n  destination : {}\n  max_budget  : {:.2} USD\n  purpose     : {}\n  \
         travel from: {}\n  travel to   : {}\n\n\
         The travel window above is fixed. Do not book a departure before {} or a \
         return after {} — those days are not part of the task, and a booking \
         outside the window will be rejected.\n\n\
         Propose one flight booking for this task as JSON with exactly these keys:\n\
         {{\n  \"task_id\": \"{}\",\n  \"destination\": \"{}\",\n  \
         \"amount\": <number, USD, at most {:.2}>,\n  \
         \"description\": \"<airline and route, e.g. 'Delta flight to NYC'>\",\n  \
         \"start_date\": \"<departure date, YYYY-MM-DD, no earlier than {}>\",\n  \
         \"end_date\": \"<return date, YYYY-MM-DD, no later than {}>\"\n}}",
        task.destination,
        task.max_budget,
        task.purpose,
        task.start_date,
        task.end_date,
        task.start_date,
        task.end_date,
        AGENT_TASK_ID,
        task.destination,
        task.max_budget,
        task.start_date,
        task.end_date,
    )
}

const TASK_SYSTEM_PROMPT: &str = "You extract a structured travel task from a user's \
     plain-English request.\n\n\
     Extract only what the user actually stated. Never invent a destination, a budget, \
     a purpose, or travel dates the user did not give — an incomplete request is a valid \
     outcome and must be reported, not filled in.\n\n\
     Travel dates are held to the same standard. A request that names no dates is \
     incomplete. So is one that names a month and a day but no year: do not assume the \
     current year, the next occurrence, or any other year the user did not say.\n\n\
     Reply with a single JSON object and nothing else — no prose, no markdown fences, \
     no explanation. Your reply is parsed directly by a program.\n\n\
     Do not include internal or system XML tags in your response.";

fn task_user_prompt(user_input: &str) -> String {
    format!(
        "USER REQUEST\n{user_input}\n\n\
         If the request states a destination, a budget, and the travel dates, reply with:\n\
         {{\n  \"destination\": \"<city name>\",\n  \"max_budget\": <number, USD>,\n  \
         \"purpose\": \"<short phrase, why they are travelling>\",\n  \
         \"start_date\": \"<first day of travel, YYYY-MM-DD>\",\n  \
         \"end_date\": \"<last day of travel, YYYY-MM-DD>\"\n}}\n\n\
         If the destination, the budget, or the dates are missing — including a date \
         given without a year — reply instead with:\n\
         {{\n  \"error\": \"<which one is missing>\"\n}}"
    )
}

// ------------------------------------------------------------- the call -----

/// Asks Claude for a booking. Fails loudly; [`propose_or_fallback`] is what the
/// scripted scenario calls.
pub async fn agent_propose_action(task: &Task) -> Result<ProposedAction, Error> {
    let text = call_claude(&system_prompt(), &user_prompt(task)).await?;
    action_from_text(&text)
}

/// Turns a plain-English request into a [`Task`].
///
/// There is deliberately no fallback: inventing a destination or a budget the
/// user never gave would defeat the point of intent validation. A vague request
/// is an error the caller reports, not a gap to fill in.
pub async fn parse_task_from_prompt(user_input: &str) -> Result<Task, Error> {
    let text = call_claude(TASK_SYSTEM_PROMPT, &task_user_prompt(user_input)).await?;
    task_from_text(&text)
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

/// Tolerates a model that wraps its JSON in prose or a markdown fence by
/// taking the outermost braced span.
fn json_span(text: &str) -> Result<&str, Error> {
    let start = text.find('{');
    let end = text.rfind('}');
    match (start, end) {
        (Some(start), Some(end)) if end > start => Ok(&text[start..=end]),
        _ => Err(Error::Parse(format!("no json object in {text:?}"))),
    }
}

/// True for a zero-padded `YYYY-MM-DD` date with a plausible month and day.
///
/// This guards the precondition `intent_check` relies on: string ordering is
/// only chronological ordering while every date has this exact shape. It is a
/// shape check, not a calendar — `2026-02-31` passes, and comparing it still
/// behaves consistently, which is all the gate needs.
fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit())
    {
        return false;
    }

    let number = |from: usize, to: usize| value[from..to].parse::<u32>().unwrap_or(0);
    (1..=12).contains(&number(5, 7)) && (1..=31).contains(&number(8, 10))
}

fn action_from_text(text: &str) -> Result<ProposedAction, Error> {
    let span = json_span(text)?;
    let mut action: ProposedAction =
        serde_json::from_str(span).map_err(|e| Error::Parse(format!("{e} in {span:?}")))?;

    // A date the gate cannot compare is worse than no date: it would sail
    // through a string comparison meaning nothing. Reject it as malformed
    // output, which is what it is.
    for (field, value) in [
        ("start_date", &action.start_date),
        ("end_date", &action.end_date),
    ] {
        if !is_iso_date(value) {
            return Err(Error::Parse(format!(
                "{field} {value:?} is not a YYYY-MM-DD date"
            )));
        }
    }

    if action.end_date < action.start_date {
        return Err(Error::Parse(format!(
            "end_date {:?} is before start_date {:?}",
            action.end_date, action.start_date
        )));
    }

    // The tracker keys on this, so it is ours to set, not the model's.
    action.task_id = AGENT_TASK_ID.to_string();
    Ok(action)
}

/// Reads a [`Task`], rejecting both an explicit "I could not extract this" and
/// a silently under-filled one.
fn task_from_text(text: &str) -> Result<Task, Error> {
    let span = json_span(text)?;
    let value: Value =
        serde_json::from_str(span).map_err(|e| Error::Parse(format!("{e} in {span:?}")))?;

    if let Some(missing) = value.get("error").and_then(Value::as_str) {
        return Err(Error::Incomplete(missing.to_string()));
    }

    let task: Task =
        serde_json::from_value(value).map_err(|e| Error::Parse(format!("{e} in {span:?}")))?;

    // A model that answers in the right shape can still answer with nothing in
    // it. Treat that as an incomplete request rather than a valid task.
    if task.destination.trim().is_empty() {
        return Err(Error::Incomplete("no destination given".to_string()));
    }
    // `is_finite` also rejects the NaN and infinity a model can emit.
    if !task.max_budget.is_finite() || task.max_budget <= 0.0 {
        return Err(Error::Incomplete("no usable budget given".to_string()));
    }
    if task.purpose.trim().is_empty() {
        return Err(Error::Incomplete("no purpose given".to_string()));
    }

    // Dates get the same treatment as destination and budget: a model that
    // could not find them must say so rather than pick some. Anything that is
    // not a usable date is reported as the missing information it stands for.
    for (field, value) in [
        ("start_date", &task.start_date),
        ("end_date", &task.end_date),
    ] {
        if !is_iso_date(value) {
            return Err(Error::Incomplete(format!(
                "no usable {field} given (got {value:?})"
            )));
        }
    }

    if task.end_date < task.start_date {
        return Err(Error::Incomplete(format!(
            "the travel window ends before it starts ({} to {})",
            task.start_date, task.end_date
        )));
    }

    Ok(task)
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
        start_date: "2026-08-15".to_string(),
        end_date: "2026-08-16".to_string(),
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

    /// Reads a response body the way the live path does: text block, then parse.
    fn action_from_body(raw: &str) -> Result<ProposedAction, Error> {
        action_from_text(&first_text_block(raw)?)
    }

    #[test]
    fn parses_a_clean_json_reply() {
        let action = action_from_body(&body(
            r#"{"task_id":"x","destination":"NYC","amount":465.0,"description":"JetBlue to NYC",
                "start_date":"2026-08-15","end_date":"2026-08-16"}"#,
        ))
        .expect("parse");
        assert_eq!(action.destination, "NYC");
        assert_eq!(action.amount, 465.0);
        assert_eq!(action.description, "JetBlue to NYC");
        assert_eq!(action.start_date, "2026-08-15");
        assert_eq!(action.end_date, "2026-08-16");
    }

    #[test]
    fn overrides_whatever_task_id_the_model_chose() {
        let action = action_from_body(&body(
            r#"{"task_id":"whatever-i-want","destination":"NYC","amount":465.0,"description":"f",
                "start_date":"2026-08-15","end_date":"2026-08-16"}"#,
        ))
        .expect("parse");
        assert_eq!(action.task_id, AGENT_TASK_ID);
    }

    #[test]
    fn tolerates_prose_and_fences_around_the_json() {
        let action = action_from_text(
            "Sure! Here you go:\n```json\n{\"task_id\":\"x\",\"destination\":\"NYC\",\
             \"amount\":470.0,\"description\":\"United to NYC\",\
             \"start_date\":\"2026-08-15\",\"end_date\":\"2026-08-16\"}\n```\nHope that helps.",
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
    fn rejects_an_action_date_that_is_not_iso() {
        for bad in ["Aug 15 2026", "2026-8-15", "15/08/2026", ""] {
            let reply = format!(
                r#"{{"task_id":"x","destination":"NYC","amount":465.0,"description":"f",
                     "start_date":"{bad}","end_date":"2026-08-16"}}"#
            );
            assert!(
                matches!(action_from_text(&reply), Err(Error::Parse(_))),
                "{bad:?} should be rejected as a date"
            );
        }
    }

    #[test]
    fn rejects_an_action_returning_before_it_departs() {
        assert!(matches!(
            action_from_text(
                r#"{"task_id":"x","destination":"NYC","amount":465.0,"description":"f",
                    "start_date":"2026-08-16","end_date":"2026-08-15"}"#
            ),
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
        assert!(matches!(action_from_body(&refused), Err(Error::Parse(_))));
    }

    // ------------------------------------------------ natural-language tasks --

    #[test]
    fn parses_a_clean_task_reply() {
        let task = task_from_text(
            r#"{"destination":"NYC","max_budget":500.0,"purpose":"hackathon attendance",
                "start_date":"2026-08-15","end_date":"2026-08-16"}"#,
        )
        .expect("parse");
        assert_eq!(task.destination, "NYC");
        assert_eq!(task.max_budget, 500.0);
        assert_eq!(task.purpose, "hackathon attendance");
        assert_eq!(task.start_date, "2026-08-15");
        assert_eq!(task.end_date, "2026-08-16");
    }

    #[test]
    fn parses_a_task_wrapped_in_prose_and_fences() {
        let task = task_from_text(
            "Got it:\n```json\n{\"destination\":\"Boston\",\"max_budget\":300.0,\
             \"purpose\":\"client visit\",\"start_date\":\"2026-09-01\",\
             \"end_date\":\"2026-09-03\"}\n```\nLet me know!",
        )
        .expect("parse");
        assert_eq!(task.destination, "Boston");
        assert_eq!(task.max_budget, 300.0);
    }

    #[test]
    fn a_missing_field_is_not_a_task() {
        assert!(matches!(
            task_from_text(r#"{"destination":"NYC","purpose":"hackathon"}"#),
            Err(Error::Parse(_))
        ));
    }

    #[test]
    fn non_json_is_not_a_task() {
        assert!(matches!(
            task_from_text("Sure, where would you like to go?"),
            Err(Error::Parse(_))
        ));
    }

    /// Dates are held to the destination-and-budget standard: unusable means
    /// incomplete, never a guess.
    #[test]
    fn a_task_without_usable_dates_is_incomplete_not_valid() {
        for bad in ["", "08-15", "August 15th", "2026-08"] {
            let reply = format!(
                r#"{{"destination":"NYC","max_budget":500.0,"purpose":"x",
                     "start_date":"{bad}","end_date":"2026-08-16"}}"#
            );
            assert!(
                matches!(task_from_text(&reply), Err(Error::Incomplete(_))),
                "{bad:?} should be reported as incomplete"
            );
        }
    }

    #[test]
    fn a_backwards_travel_window_is_incomplete_not_valid() {
        assert!(matches!(
            task_from_text(
                r#"{"destination":"NYC","max_budget":500.0,"purpose":"x",
                    "start_date":"2026-08-16","end_date":"2026-08-15"}"#
            ),
            Err(Error::Incomplete(_))
        ));
    }

    /// The booking agent can only respect a window it was told about, so the
    /// prompt has to carry the task's real dates.
    #[test]
    fn the_booking_prompt_states_the_task_window() {
        let task = Task {
            destination: "NYC".to_string(),
            max_budget: 500.0,
            purpose: "hackathon attendance".to_string(),
            start_date: "2026-08-15".to_string(),
            end_date: "2026-08-16".to_string(),
        };
        let prompt = user_prompt(&task);
        assert!(prompt.contains("2026-08-15"), "{prompt}");
        assert!(prompt.contains("2026-08-16"), "{prompt}");
    }

    #[test]
    fn iso_date_accepts_only_zero_padded_full_dates() {
        assert!(is_iso_date("2026-08-15"));
        assert!(is_iso_date("2026-12-31"));
        assert!(!is_iso_date("2026-13-01"));
        assert!(!is_iso_date("2026-08-32"));
        assert!(!is_iso_date("2026-08-15T00:00:00Z"));
    }

    /// The whole point: a vague request must not be filled in with a guess.
    #[test]
    fn an_explicit_error_reply_is_reported_as_incomplete() {
        assert!(matches!(
            task_from_text(r#"{"error":"no budget was stated"}"#),
            Err(Error::Incomplete(_))
        ));
    }

    #[test]
    fn an_empty_destination_is_incomplete_not_valid() {
        assert!(matches!(
            task_from_text(
                r#"{"destination":"  ","max_budget":500.0,"purpose":"x",
                    "start_date":"2026-08-15","end_date":"2026-08-16"}"#
            ),
            Err(Error::Incomplete(_))
        ));
    }

    #[test]
    fn a_zero_or_negative_budget_is_incomplete_not_valid() {
        for budget in ["0.0", "-25.0"] {
            let reply = format!(
                r#"{{"destination":"NYC","max_budget":{budget},"purpose":"x",
                     "start_date":"2026-08-15","end_date":"2026-08-16"}}"#
            );
            assert!(
                matches!(task_from_text(&reply), Err(Error::Incomplete(_))),
                "budget {budget} should be rejected"
            );
        }
    }

    #[test]
    fn fallback_matches_the_hardcoded_scenario_one_booking() {
        let action = fallback_action();
        assert_eq!(action.destination, "NYC");
        assert_eq!(action.amount, 480.0);
        assert_eq!(action.description, "Delta flight to NYC");
        assert_eq!(action.start_date, "2026-08-15");
        assert_eq!(action.end_date, "2026-08-16");
    }
}
