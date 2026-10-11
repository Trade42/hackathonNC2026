//! The agent's log, `ferry.agent_turns`: one row per inbound event the bot answers, claimed
//! before the turn starts and finished with its reply, every Gemini and tool call with its
//! latency (`tool_calls`), the model and the turn's own latency.
//!
//! The claim is the unique `inbound_key`, a hash of the event, so an event the sidecar or
//! Telegram delivers twice is answered once. Group messages the bot stays silent on are never
//! logged.

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// One Gemini or tool call within a turn, as `agent_turns.tool_calls` stores it.
#[derive(Debug, Clone, Serialize)]
pub struct CallLog {
    /// A tool's name, or `gemini` for a generateContent call.
    pub name: String,
    /// The tool's arguments, or for Gemini `{round, attempts}`.
    pub args: Value,
    pub ok: bool,
    pub latency_ms: u64,
    /// A short reason when `ok` is false (an error label, never a payload).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The inbound event's identity: SHA-256 over its channel, kind, chat, sender, time, text and
/// button data, as 64 hex digits.
pub fn inbound_key(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        // Length-prefixed, so ("ab", "c") and ("a", "bc") differ.
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// What a turn answers.
#[derive(Debug, Clone)]
pub struct Claim<'a> {
    pub inbound_key: &'a str,
    pub space_id: &'a str,
    pub user_id: Option<&'a str>,
    pub is_group: bool,
    pub inbound_text: &'a str,
}

/// Claims the event: the new turn's id, or None when another delivery of the same event already
/// claimed it (then the bot stays silent).
pub async fn claim(pool: &PgPool, claim: &Claim<'_>) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO ferry.agent_turns (inbound_key, space_id, user_id, is_group, inbound_text)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (inbound_key) DO NOTHING
         RETURNING id",
    )
    .bind(claim.inbound_key)
    .bind(claim.space_id)
    .bind(claim.user_id)
    .bind(claim.is_group)
    .bind(claim.inbound_text)
    .fetch_optional(pool)
    .await
}

/// How a turn ended.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    /// What the bot said, joined; None when it stayed silent.
    pub reply_text: Option<String>,
    pub calls: Vec<CallLog>,
    /// The Gemini model, when Gemini was called.
    pub model: Option<String>,
    /// Why the turn fell back or failed, when it did.
    pub error: Option<String>,
}

/// Finishes turn `id`. A failure is logged and otherwise ignored: the log never stops a reply.
pub async fn finish(pool: &PgPool, id: i64, outcome: &Outcome, latency_ms: u64) {
    let calls = serde_json::to_value(&outcome.calls).unwrap_or_else(|_| Value::Array(Vec::new()));
    let result = sqlx::query(
        "UPDATE ferry.agent_turns
            SET reply_text = $2, tool_calls = $3, model = $4, latency_ms = $5, error = $6
          WHERE id = $1",
    )
    .bind(id)
    .bind(outcome.reply_text.as_deref())
    .bind(calls)
    .bind(outcome.model.as_deref())
    .bind(i32::try_from(latency_ms).unwrap_or(i32::MAX))
    .bind(outcome.error.as_deref())
    .execute(pool)
    .await;
    if let Err(err) = result {
        tracing::warn!(turn = id, "agent turn log not written: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_hex_and_length_prefixed() {
        let key = inbound_key(&["telegram", "message", "-100", "1", "t", "hi"]);
        assert_eq!(key.len(), 64);
        assert!(
            key.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_ne!(inbound_key(&["ab", "c"]), inbound_key(&["a", "bc"]));
        assert_eq!(
            key,
            inbound_key(&["telegram", "message", "-100", "1", "t", "hi"])
        );
    }
}
