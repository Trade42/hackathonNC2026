//! `POST /internal/messages/inbound`: each event the sidecar forwards (messaging/README.md,
//! Inbound). Answers fast with `{replies: [{space_id, text}]}` and never waits for Gemini.
//!
//! The dispatcher handles Telegram sign-in first, exactly as before: a DM `/start [code]` and the
//! presses of the sign-in prompt's buttons. Lines without buttons go back as replies; a message
//! with buttons goes out through the sidecar's `POST /send` after the answer (replies cannot carry
//! buttons). Every other Telegram event goes to the agent ([`crate::agent::dispatch`]), which
//! answers `{replies: []}` at once and sends what the bot says through `/send` from its own task.

use axum::extract::{FromRequest, Request, State};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use super::mask::mask_space_id;
use super::sidecar::{self, SECRET_HEADER, SendRequest};
use crate::agent;
use crate::auth::telegram::{self, TelegramSender, is_telegram_user_id};
use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::extract::Json;
use crate::state::AppState;

/// The fields of the sidecar's inbound payload this dispatcher reads; the rest are ignored.
/// Everything is optional, so a v1 payload (without `kind` and `channel`) still parses.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct InboundEvent {
    /// `message` (the default), `callback` or `membership`.
    pub kind: Option<String>,
    /// `telegram` or `imessage`.
    pub channel: Option<String>,
    pub space_id: Option<String>,
    pub is_group: bool,
    /// The Telegram user id, as a string.
    pub sender_id: Option<String>,
    /// Without `@`.
    pub sender_username: Option<String>,
    /// The Telegram first name. Read only so the agent can keep it from Gemini; never stored,
    /// logged or sent anywhere.
    pub sender_name: Option<String>,
    /// A group's title; null in a DM.
    pub chat_title: Option<String>,
    pub text: Option<String>,
    /// When the message was sent (for a callback, when the sidecar got it); part of the agent's
    /// key for "never answer twice".
    pub received_at: Option<String>,
    /// For `kind: "callback"`: the pressed button's data.
    pub callback_data: Option<String>,
    /// For `kind: "membership"`: whether the bot is in the chat now (in a DM, false means the
    /// user blocked it).
    pub bot_in_chat: Option<bool>,
    /// For `kind: "membership"`: whether the bot is an administrator now; null when unknown.
    pub bot_is_admin: Option<bool>,
    /// For `kind: "membership"` after a group became a supergroup: its old chat id.
    pub migrated_from_space_id: Option<String>,
    /// Whether the message replies to one of the bot's. The sidecar does not send this field
    /// yet; when it does, a reply to the bot counts as addressing it in a group. Null or absent
    /// is false, so a `null` never refuses the event.
    pub reply_to_bot: Option<bool>,
}

/// One reply the sidecar sends into `space_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reply {
    pub space_id: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct InboundResponse {
    pub replies: Vec<Reply>,
}

/// True when `given` is `SIDECAR_SHARED_SECRET`, compared in constant time (HMAC tags of both,
/// checked with `verify_slice`, so neither the length nor a prefix leaks). False when the secret
/// is unset: then nothing is accepted.
pub fn secret_matches(config: &Config, given: Option<&[u8]>) -> bool {
    let (Some(secret), Some(given)) = (&config.sidecar_shared_secret, given) else {
        return false;
    };
    const KEY: &[u8] = b"ferry inbound secret check v1";
    let tag = |value: &[u8]| {
        let mut mac = Hmac::<Sha256>::new_from_slice(KEY).expect("HMAC takes a key of any length");
        mac.update(value);
        mac
    };
    let given_tag = tag(given).finalize().into_bytes();
    tag(secret.expose().as_bytes())
        .verify_slice(&given_tag)
        .is_ok()
}

/// `/start`, `/start <payload>` or `/start@<bot> <payload>`: Some(payload) for the start
/// command, None for any other text.
pub fn start_payload(text: &str) -> Option<Option<&str>> {
    let text = text.trim();
    let (command, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    if command != "/start" && !command.starts_with("/start@") {
        return None;
    }
    let rest = rest.trim();
    Some((!rest.is_empty()).then_some(rest))
}

/// 401 `unauthorized` without the right `X-Ferry-Secret`. The handler takes the whole request
/// and checks the header first, so the body of a refused request is never read, buffered or
/// parsed; only then does the JSON extractor read it (400 `validation_failed` as everywhere).
pub async fn inbound(
    State(state): State<AppState>,
    request: Request,
) -> ApiResult<Json<InboundResponse>> {
    let given = request.headers().get(SECRET_HEADER).map(|v| v.as_bytes());
    if !secret_matches(&state.config, given) {
        if state.config.sidecar_shared_secret.is_none() {
            tracing::warn!("inbound refused: SIDECAR_SHARED_SECRET is unset");
        }
        return Err(ApiError::unauthorized("Wrong or missing X-Ferry-Secret."));
    }
    let Json(event) = Json::<InboundEvent>::from_request(request, &state).await?;
    let mut replies = Vec::new();
    for message in dispatch(&state, &event).await? {
        if message.buttons.is_empty() {
            replies.push(Reply {
                space_id: message.space_id,
                text: message.text,
            });
        } else {
            sidecar::spawn_send(&state, message);
        }
    }
    Ok(Json(InboundResponse { replies }))
}

/// What the bot says about `event`, to send.
async fn dispatch(state: &AppState, event: &InboundEvent) -> ApiResult<Vec<SendRequest>> {
    if event.channel.as_deref() != Some("telegram") {
        return Ok(Vec::new());
    }
    let (Some(sender_id), Some(space_id)) = (event.sender_id.as_deref(), event.space_id.as_deref())
    else {
        return Ok(Vec::new());
    };
    if !is_telegram_user_id(sender_id) {
        return Ok(Vec::new());
    }
    let sender = TelegramSender {
        id: sender_id,
        username: event.sender_username.as_deref().filter(|u| !u.is_empty()),
        space_id,
    };
    let kind = event.kind.as_deref().unwrap_or("message");
    match kind {
        // Sign-in happens in the DM. In a group, `/start@<bot> <code>` comes from a startgroup
        // link, which is the group mapping's business (the agent's).
        "message" if !event.is_group => {
            if let Some(payload) = event.text.as_deref().and_then(start_payload) {
                return telegram::on_start(state, &sender, payload).await;
            }
        }
        "callback" => {
            if let Some((token, confirm)) = event
                .callback_data
                .as_deref()
                .and_then(telegram::parse_button)
            {
                tracing::debug!(space = %mask_space_id(space_id), "sign-in button pressed");
                return telegram::on_button(state, &sender, token, confirm).await;
            }
        }
        _ => {}
    }
    let agent_event = agent::dispatch::Event {
        raw: event,
        kind,
        space_id,
        sender_id,
    };
    agent::dispatch::dispatch(state, &agent_event).await?;
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(secret: Option<&str>) -> Config {
        let mut vars = HashMap::from([(
            "DATABASE_URL".to_owned(),
            "postgres://u:p@127.0.0.1/db".to_owned(),
        )]);
        if let Some(secret) = secret {
            vars.insert("SIDECAR_SHARED_SECRET".into(), secret.into());
        }
        Config::from_vars(&vars).unwrap()
    }

    #[test]
    fn the_secret_must_match_exactly() {
        let c = config(Some("s3cret-value"));
        assert!(secret_matches(&c, Some(b"s3cret-value")));
        for wrong in [&b""[..], b"s3cret", b"s3cret-value ", b"S3cret-value", b"x"] {
            assert!(!secret_matches(&c, Some(wrong)), "{wrong:?}");
        }
        assert!(!secret_matches(&c, None));
        // Unset: nothing matches, not even an empty header.
        let unset = config(None);
        assert!(!secret_matches(&unset, Some(b"")));
        assert!(!secret_matches(&unset, None));
    }

    #[test]
    fn start_commands() {
        assert_eq!(start_payload("/start"), Some(None));
        assert_eq!(start_payload("  /start  "), Some(None));
        assert_eq!(start_payload("/start abcDEF123"), Some(Some("abcDEF123")));
        assert_eq!(start_payload("/start@Ferrygo_bot abc"), Some(Some("abc")));
        assert_eq!(start_payload("/start   a b"), Some(Some("a b")));
        for other in ["", "start abc", "/started", "/stop", "hi /start abc"] {
            assert_eq!(start_payload(other), None, "{other}");
        }
    }

    #[test]
    fn v1_payloads_parse() {
        let event: InboundEvent = serde_json::from_value(serde_json::json!({
            "space_id": "any;-;+19195550102", "is_group": false, "participants": [],
            "sender_phone": "+19195550102", "text": "hi", "attachments": [],
            "location": null, "received_at": "2026-10-10T22:00:00.000Z"
        }))
        .unwrap();
        assert_eq!(event.kind, None);
        assert_eq!(event.channel, None);
        assert_eq!(event.text.as_deref(), Some("hi"));
    }
}
