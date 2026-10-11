//! Telegram sign-in (contract v1.3.1, with Sal's Confirm step of 2026-10-10, 22:12 EDT and the
//! frontend's matching confirm code).
//!
//! 1. `POST /v1/auth/telegram/start` makes a challenge and returns `bot_url`,
//!    `https://t.me/<bot>?start=<code>`, and a four-digit `confirm_code` the client shows.
//! 2. The user taps Start, and the sidecar forwards `/start <code>` from their Telegram user id.
//!    [`on_start`] binds the challenge to that id and the bot asks "Sign in to Ferry on another
//!    device? Check that the screen shows 4821." with the same confirm code, and Confirm and Not me
//!    buttons carrying a random confirm token.
//! 3. A Confirm press from the same Telegram id ([`on_button`]) verifies the challenge for that
//!    id's user, creating it on first sight; Not me expires the challenge.
//! 4. `GET /v1/auth/telegram/{challenge_id}` answers pending until then, and hands out a token once,
//!    on the first poll within a short time after verification.
//!
//! Without the Confirm step, whoever opened a sign-in link someone else started would sign that
//! person's device in as themselves: an attacker could send a victim a fresh bot_url and collect
//! the victim's session from the poll. Binding alone does not close that; the explicit question,
//! answered by the bound account only, does, and the code makes the question concrete: the victim
//! sees a code that matches nothing on their own screen.

use axum::extract::State;
use axum::http::HeaderMap;
use rand::RngExt;
use serde::Serialize;
use sqlx::PgConnection;

use super::limits::{self, client_ip};
use crate::catalog::public_config::DEFAULT_WEB_URL;
use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path};
use crate::ids;
use crate::messaging::mask::{mask_telegram_id, mask_telegram_username};
use crate::messaging::sidecar::{Button, SendRequest};
use crate::profile::{User, load_user};
use crate::state::AppState;

/// `pg_advisory_xact_lock` key that serializes starts, so the limits' counts and the insert are
/// one step: two concurrent starts never both see room for one more.
const START_LOCK_KEY: i64 = 7_202_610_110_002;

/// New users' bubble colors, one picked at random.
const BUBBLE_COLORS: [&str; 8] = [
    "#2563EB", "#F97316", "#16A34A", "#9333EA", "#DB2777", "#0891B2", "#CA8A04", "#DC2626",
];

pub const CHALLENGE_ID_PREFIX: &str = "chl_";
/// Random characters after `chl_` from `[0-9a-z]`: about 165 bits, as hard to guess as a token,
/// since the id is all the poll needs.
pub const CHALLENGE_ID_RANDOM_LEN: usize = 32;
/// The start code's length, from `[A-Za-z0-9]`: about 143 bits. Telegram allows 64 characters
/// from `[A-Za-z0-9_-]` in a start parameter.
pub const START_CODE_LEN: usize = 24;
/// The confirm token's length, from `[A-Za-z0-9]`: about 131 bits.
pub const CONFIRM_TOKEN_LEN: usize = 22;
/// The confirm code's length, in decimal digits. It is compared by eye, not guessed, so it is short.
pub const CONFIRM_CODE_LEN: usize = 4;

/// The callback data of the prompt's buttons: `signin:<confirm token>:confirm` or `:cancel`
/// (37 bytes at most, under Telegram's 64). Not me sends `cancel`, as #17's Cancel did, so prompts
/// sent before the label changed still work.
pub const BUTTON_PREFIX: &str = "signin";
pub const CONFIRM_LABEL: &str = "Confirm";
pub const NOT_ME_LABEL: &str = "Not me";

const DIGITS: &[u8; 10] = b"0123456789";

const LOWER_ALNUM: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const ALNUM: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

// What the bot says. One line each.

/// The question with the buttons: "Sign in to Ferry on another device? Check that the screen
/// shows 4821."
pub fn prompt_text(confirm_code: &str) -> String {
    format!("Sign in to Ferry on another device? Check that the screen shows {confirm_code}.")
}

pub const WELCOME: &str = "Welcome to Ferry! Open the app to sign in and order with your friends.";
pub const LINK_USED: &str =
    "This sign-in link has expired or was already used. Start again in the Ferry app.";
pub const LINK_TAKEN: &str = "This sign-in link is already in use by another Telegram account. Start again in the Ferry app.";
pub const SIGNED_IN: &str = "You're signed in to Ferry. Go back to the app.";
pub const CANCELLED: &str = "Sign-in cancelled. Nothing was signed in.";
pub const NOT_YOURS: &str = "This button belongs to someone else's sign-in, so it did nothing.";
pub const ALREADY_SIGNED_IN: &str = "You're already signed in. Go back to the Ferry app.";
pub const EXPIRED: &str = "This sign-in has expired. Start again in the Ferry app.";
pub const BUTTON_GONE: &str = "This sign-in button no longer works. Start again in the Ferry app.";

/// `TelegramAuthChallenge`.
#[derive(Debug, Serialize)]
pub struct TelegramAuthChallenge {
    pub challenge_id: String,
    pub bot_url: String,
    pub expires_at: String,
    pub confirm_code: String,
}

/// `TelegramAuthStatus`: `token`, `user` and `is_new` only on the one poll that collects a verified
/// challenge's token.
#[derive(Debug, Serialize)]
pub struct TelegramAuthStatus {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_new: Option<bool>,
}

fn random_string(alphabet: &[u8], len: usize) -> String {
    let mut rng = rand::rng();
    (0..len)
        .map(|_| alphabet[rng.random_range(0..alphabet.len())] as char)
        .collect()
}

/// A new opaque challenge id, `chl_` plus 32 characters from `[0-9a-z]`.
pub fn new_challenge_id() -> String {
    format!(
        "{CHALLENGE_ID_PREFIX}{}",
        random_string(LOWER_ALNUM, CHALLENGE_ID_RANDOM_LEN)
    )
}

pub fn new_start_code() -> String {
    random_string(ALNUM, START_CODE_LEN)
}

pub fn new_confirm_token() -> String {
    random_string(ALNUM, CONFIRM_TOKEN_LEN)
}

/// Four random decimal digits, leading zeros kept: `0482`.
pub fn new_confirm_code() -> String {
    random_string(DIGITS, CONFIRM_CODE_LEN)
}

fn is_token_chars(value: &str, lengths: std::ops::RangeInclusive<usize>) -> bool {
    lengths.contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Shaped like a start code (the schema's CHECK): 16 to 64 characters from `[A-Za-z0-9_-]`.
pub fn is_start_code(value: &str) -> bool {
    is_token_chars(value, 16..=64)
}

/// A Telegram user id as the sidecar sends it: a positive decimal (the schema's CHECK).
pub fn is_telegram_user_id(value: &str) -> bool {
    (1..=20).contains(&value.len())
        && value.bytes().all(|b| b.is_ascii_digit())
        && !value.starts_with('0')
}

/// A Telegram username as `users.telegram_username` holds it: without `@`, `[A-Za-z0-9_]`.
fn is_telegram_username(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The prompt's callback data.
pub fn button_data(confirm_token: &str, confirm: bool) -> String {
    let action = if confirm { "confirm" } else { "cancel" };
    format!("{BUTTON_PREFIX}:{confirm_token}:{action}")
}

/// Reads `signin:<token>:confirm|cancel` back into the token and whether it confirms. None for
/// any other callback data, which belongs to someone else (`invite:...`).
pub fn parse_button(callback_data: &str) -> Option<(&str, bool)> {
    let mut parts = callback_data.split(':');
    let (prefix, token, action) = (parts.next()?, parts.next()?, parts.next()?);
    if prefix != BUTTON_PREFIX || parts.next().is_some() {
        return None;
    }
    match action {
        "confirm" => Some((token, true)),
        "cancel" => Some((token, false)),
        _ => None,
    }
}

// ---------- The app's side ----------

#[derive(sqlx::FromRow)]
struct Counts {
    ip: i64,
    global: i64,
}

/// `POST /v1/auth/telegram/start`: checks the limits and makes a pending challenge. Sends
/// nothing: the bot speaks only once the user taps Start. 503 without `TELEGRAM_BOT_USERNAME`.
pub async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<TelegramAuthChallenge>> {
    let Some(bot) = state.config.telegram_bot_username.as_deref() else {
        return Err(ApiError::upstream_unavailable(
            "Telegram sign-in is not set up on this server.",
        ));
    };
    let ip = client_ip(&headers);
    let challenge_id = new_challenge_id();
    let code = new_start_code();
    let confirm_code = new_confirm_code();

    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(START_LOCK_KEY)
        .execute(&mut *tx)
        .await?;
    let counts: Counts = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE client_ip = $1) AS ip, count(*) AS global
           FROM ferry.telegram_challenges
          WHERE created_at > now() - make_interval(mins => $2)",
    )
    .bind(&ip)
    .bind(limits::WINDOW_MINUTES)
    .fetch_one(&mut *tx)
    .await?;
    let refusal = if counts.ip >= limits::PER_IP {
        Some((
            "ip",
            "Too many sign-ins from this network. Try again in a few minutes.",
        ))
    } else if counts.global >= limits::GLOBAL {
        Some((
            "global",
            "Too many people are signing in right now. Try again in a few minutes.",
        ))
    } else {
        None
    };
    if let Some((limit, message)) = refusal {
        tracing::warn!(limit, "Telegram sign-in start refused: rate limit");
        return Err(ApiError::rate_limited(message));
    }
    let expires_at: String = sqlx::query_scalar(
        "INSERT INTO ferry.telegram_challenges (id, start_code, client_ip, confirm_code, expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(mins => $5))
         RETURNING to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')",
    )
    .bind(&challenge_id)
    .bind(&code)
    .bind(&ip)
    .bind(&confirm_code)
    .bind(limits::CHALLENGE_TTL_MINUTES)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!("Telegram sign-in started");
    Ok(Json(TelegramAuthChallenge {
        challenge_id,
        bot_url: format!("https://t.me/{bot}?start={code}"),
        expires_at,
        confirm_code,
    }))
}

#[derive(sqlx::FromRow)]
struct StatusRow {
    status: String,
    time_expired: bool,
    /// Verified, within [`limits::TOKEN_POLL_SECONDS`] of it, and the token not yet handed out.
    token_ready: bool,
}

#[derive(sqlx::FromRow)]
struct Collected {
    user_id: String,
    is_new: bool,
}

/// `GET /v1/auth/telegram/{challenge_id}`: pending (also while the bot waits for Confirm),
/// verified or expired. A verified challenge carries a fresh token, the user and is_new once: on
/// the first poll within [`limits::TOKEN_POLL_SECONDS`] after verification. Every other poll
/// answers only its status. Tokens are never stored.
pub async fn status(
    State(state): State<AppState>,
    Path(challenge_id): Path<String>,
) -> ApiResult<Json<TelegramAuthStatus>> {
    let row: Option<StatusRow> = sqlx::query_as(
        "SELECT status, expires_at <= now() AS time_expired,
                (token_issued_at IS NULL
                 AND COALESCE(verified_at > now() - make_interval(secs => $2), false)) AS token_ready
           FROM ferry.telegram_challenges
          WHERE id = $1",
    )
    .bind(&challenge_id)
    .bind(f64::from(limits::TOKEN_POLL_SECONDS))
    .fetch_optional(&state.pool)
    .await?;
    let Some(row) = row else {
        return Err(ApiError::not_found("No sign-in challenge has this id."));
    };
    let answer = match row.status.as_str() {
        "verified" if row.token_ready => collect_token(&state, &challenge_id)
            .await?
            .unwrap_or_else(|| bare_status("verified")),
        "verified" => bare_status("verified"),
        "pending" if !row.time_expired => bare_status("pending"),
        _ => bare_status("expired"),
    };
    Ok(Json(answer))
}

fn bare_status(status: &'static str) -> TelegramAuthStatus {
    TelegramAuthStatus {
        status,
        token: None,
        user: None,
        is_new: None,
    }
}

/// Hands out a verified challenge's token, once. One transaction marks it handed out
/// (`token_issued_at`, set only where it is still null), loads the user and signs the token, so a
/// failure before the commit leaves the token for the next poll, and of two polls at the same
/// moment only one gets it. None when another poll got it first or the window has closed.
async fn collect_token(
    state: &AppState,
    challenge_id: &str,
) -> ApiResult<Option<TelegramAuthStatus>> {
    let mut tx = state.pool.begin().await?;
    let collected: Option<Collected> = sqlx::query_as(
        "UPDATE ferry.telegram_challenges
            SET token_issued_at = now()
          WHERE id = $1
            AND status = 'verified'
            AND token_issued_at IS NULL
            AND verified_at > now() - make_interval(secs => $2)
         RETURNING user_id, is_new",
    )
    .bind(challenge_id)
    .bind(f64::from(limits::TOKEN_POLL_SECONDS))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(Collected { user_id, is_new }) = collected else {
        return Ok(None);
    };
    let user = load_user(&mut *tx, &user_id)
        .await?
        .ok_or_else(|| ApiError::internal("verified challenge without its user"))?;
    let token = state.jwt.issue_token(&user_id)?;
    tx.commit().await?;
    Ok(Some(TelegramAuthStatus {
        status: "verified",
        token: Some(token),
        user: Some(user),
        is_new: Some(is_new),
    }))
}

// ---------- The bot's side ----------

/// Who pressed or wrote, from the sidecar's inbound event.
#[derive(Debug, Clone, Copy)]
pub struct TelegramSender<'a> {
    /// The Telegram user id (checked with [`is_telegram_user_id`]).
    pub id: &'a str,
    /// Without `@`; None when the account has none.
    pub username: Option<&'a str>,
    /// The chat to answer in: the DM, whose id is the user's.
    pub space_id: &'a str,
}

/// A line to `sender`'s chat, without buttons.
fn line(sender: &TelegramSender<'_>, text: &str) -> SendRequest {
    SendRequest {
        space_id: sender.space_id.to_owned(),
        text: text.to_owned(),
        buttons: Vec::new(),
    }
}

/// A line with the web app's link: a link button when `web_url` is https, otherwise the URL at
/// the end of the text, because Telegram refuses link buttons to local addresses (the dev
/// default, `http://localhost:5173`).
fn line_with_app_link(config: &Config, sender: &TelegramSender<'_>, text: &str) -> SendRequest {
    let web_url = config.public_web_url.as_deref().unwrap_or(DEFAULT_WEB_URL);
    if web_url.starts_with("https://") {
        SendRequest {
            buttons: vec![Button::url("Open Ferry", web_url)],
            ..line(sender, text)
        }
    } else {
        line(sender, &format!("{text} {web_url}"))
    }
}

/// The question, with the challenge's confirm code and the Confirm and Not me buttons.
fn prompt(sender: &TelegramSender<'_>, confirm_code: &str, confirm_token: &str) -> SendRequest {
    SendRequest {
        buttons: vec![
            Button::callback(CONFIRM_LABEL, button_data(confirm_token, true)),
            Button::callback(NOT_ME_LABEL, button_data(confirm_token, false)),
        ],
        ..line(sender, &prompt_text(confirm_code))
    }
}

#[derive(sqlx::FromRow)]
struct BindRow {
    id: String,
    status: String,
    telegram_user_id: Option<String>,
    confirm_token: Option<String>,
    confirm_code: String,
    time_expired: bool,
}

/// A Telegram DM `/start [payload]` from `sender`. A payload that is a pending challenge's code
/// binds the challenge to `sender` and asks them to confirm, showing the challenge's confirm code; it
/// verifies nothing. The same `/start <code>` again from the same account asks again with the same
/// code and token. Anything else
/// gets one line. What the bot says comes back as messages to send.
pub async fn on_start(
    state: &AppState,
    sender: &TelegramSender<'_>,
    payload: Option<&str>,
) -> ApiResult<Vec<SendRequest>> {
    let who = mask_telegram_id(sender.id);
    let Some(code) = payload.filter(|p| is_start_code(p)) else {
        return Ok(vec![line_with_app_link(&state.config, sender, WELCOME)]);
    };
    let mut tx = state.pool.begin().await?;
    let row: Option<BindRow> = sqlx::query_as(
        "SELECT id, status, telegram_user_id, confirm_token, confirm_code,
                expires_at <= now() AS time_expired
           FROM ferry.telegram_challenges
          WHERE start_code = $1
            FOR UPDATE",
    )
    .bind(code)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tracing::info!(tg = %who, "Telegram /start with an unknown code");
        return Ok(vec![line_with_app_link(&state.config, sender, WELCOME)]);
    };
    if row.status != "pending" || row.time_expired {
        if row.status == "pending" {
            expire(&mut tx, &row.id).await?;
            tx.commit().await?;
        }
        tracing::info!(tg = %who, status = row.status, "Telegram /start for a finished challenge");
        return Ok(vec![line_with_app_link(&state.config, sender, LINK_USED)]);
    }
    match (row.telegram_user_id.as_deref(), row.confirm_token) {
        (None, _) => {
            let token = new_confirm_token();
            sqlx::query(
                "UPDATE ferry.telegram_challenges
                    SET telegram_user_id = $2, confirm_token = $3, bound_at = now()
                  WHERE id = $1",
            )
            .bind(&row.id)
            .bind(sender.id)
            .bind(&token)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            tracing::info!(tg = %who, "Telegram sign-in bound; asking to confirm");
            Ok(vec![prompt(sender, &row.confirm_code, &token)])
        }
        (Some(bound), Some(token)) if bound == sender.id => {
            tracing::info!(tg = %who, "Telegram /start again; asking to confirm again");
            Ok(vec![prompt(sender, &row.confirm_code, &token)])
        }
        (Some(bound), _) => {
            tracing::warn!(
                tg = %who,
                bound = %mask_telegram_id(bound),
                "Telegram /start for a challenge bound to another account"
            );
            Ok(vec![line(sender, LINK_TAKEN)])
        }
    }
}

#[derive(sqlx::FromRow)]
struct ButtonRow {
    id: String,
    status: String,
    telegram_user_id: Option<String>,
    time_expired: bool,
}

/// A press of the prompt's Confirm or Not me (callback data from [`button_data`]; Not me is
/// `cancel`) by `sender`. Only the account the challenge is bound to can confirm it or expire it,
/// once, before it expires; every other press changes nothing and gets one line.
pub async fn on_button(
    state: &AppState,
    sender: &TelegramSender<'_>,
    confirm_token: &str,
    confirm: bool,
) -> ApiResult<Vec<SendRequest>> {
    let who = mask_telegram_id(sender.id);
    if !is_token_chars(confirm_token, 16..=32) {
        return Ok(vec![line(sender, BUTTON_GONE)]);
    }
    let mut tx = state.pool.begin().await?;
    let row: Option<ButtonRow> = sqlx::query_as(
        "SELECT id, status, telegram_user_id, expires_at <= now() AS time_expired
           FROM ferry.telegram_challenges
          WHERE confirm_token = $1
            FOR UPDATE",
    )
    .bind(confirm_token)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tracing::info!(tg = %who, "Telegram sign-in button with an unknown token");
        return Ok(vec![line(sender, BUTTON_GONE)]);
    };
    if row.telegram_user_id.as_deref() != Some(sender.id) {
        tracing::warn!(tg = %who, "Telegram sign-in button pressed by another account");
        return Ok(vec![line(sender, NOT_YOURS)]);
    }
    match row.status.as_str() {
        "verified" => {
            return Ok(vec![line_with_app_link(
                &state.config,
                sender,
                ALREADY_SIGNED_IN,
            )]);
        }
        "pending" if !row.time_expired => {}
        status => {
            if status == "pending" {
                expire(&mut tx, &row.id).await?;
                tx.commit().await?;
            }
            return Ok(vec![line(sender, EXPIRED)]);
        }
    }
    if !confirm {
        expire(&mut tx, &row.id).await?;
        tx.commit().await?;
        tracing::info!(tg = %who, "Telegram sign-in refused with Not me");
        return Ok(vec![line(sender, CANCELLED)]);
    }

    let username = sender.username.filter(|u| is_telegram_username(u));
    let (user_id, is_new) = user_for_telegram(&mut tx, &state.config, sender.id, username).await?;
    sqlx::query(
        "UPDATE ferry.telegram_challenges
            SET status = 'verified', user_id = $2, is_new = $3, verified_at = now()
          WHERE id = $1",
    )
    .bind(&row.id)
    .bind(&user_id)
    .bind(is_new)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(tg = %who, user_id, is_new, "signed in with Telegram");
    Ok(vec![line_with_app_link(&state.config, sender, SIGNED_IN)])
}

async fn expire(tx: &mut PgConnection, challenge_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE ferry.telegram_challenges SET status = 'expired' WHERE id = $1")
        .bind(challenge_id)
        .execute(tx)
        .await?;
    Ok(())
}

/// The Ferry user for a Telegram account that just confirmed a sign-in, and whether this created
/// it. In order:
/// 1. the user already linked to `telegram_user_id`, whose username is refreshed;
/// 2. in DEMO_MODE, the seeded persona `DEMO_TELEGRAM_PERSONAS` lists for `username`, which is
///    linked to the account (replacing any account linked to it before);
/// 3. otherwise a new user, with an empty display name, `avatar_seed` = its id, a random bubble
///    color and a default profiles row.
pub async fn user_for_telegram(
    tx: &mut PgConnection,
    config: &Config,
    telegram_user_id: &str,
    username: Option<&str>,
) -> ApiResult<(String, bool)> {
    let linked: Option<String> = sqlx::query_scalar(
        "UPDATE ferry.users SET telegram_username = $2
          WHERE telegram_user_id = $1
         RETURNING id",
    )
    .bind(telegram_user_id)
    .bind(username)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = linked {
        return Ok((id, false));
    }

    if let Some(persona) = username.and_then(|u| config.demo_persona_for_telegram(u)) {
        let persona_user: Option<String> = sqlx::query_scalar(
            "UPDATE ferry.users SET telegram_user_id = $2, telegram_username = $3
              WHERE id = $1
             RETURNING id",
        )
        .bind(persona.user_id())
        .bind(telegram_user_id)
        .bind(username)
        .fetch_optional(&mut *tx)
        .await?;
        match persona_user {
            Some(id) => {
                tracing::info!(
                    tg = %mask_telegram_id(telegram_user_id),
                    username = %mask_telegram_username(username.unwrap_or_default()),
                    user_id = id,
                    "Telegram account linked to its demo persona"
                );
                return Ok((id, false));
            }
            None => tracing::warn!(
                persona = persona.name(),
                "DEMO_TELEGRAM_PERSONAS names a persona that is not seeded; making a new user"
            ),
        }
    }

    // ON CONFLICT covers two first sign-ins of one account confirmed at the same moment: the
    // second waits for the first and finds its user.
    let new_user_id = ids::new_id(ids::USER);
    let color = BUBBLE_COLORS[rand::rng().random_range(0..BUBBLE_COLORS.len())];
    let created: Option<String> = sqlx::query_scalar(
        "INSERT INTO ferry.users (id, telegram_user_id, telegram_username, avatar_seed, bubble_color)
         VALUES ($1, $2, $3, $1, $4)
         ON CONFLICT (telegram_user_id) DO NOTHING
         RETURNING id",
    )
    .bind(&new_user_id)
    .bind(telegram_user_id)
    .bind(username)
    .bind(color)
    .fetch_optional(&mut *tx)
    .await?;
    match created {
        Some(id) => {
            sqlx::query("INSERT INTO ferry.profiles (user_id) VALUES ($1) ON CONFLICT DO NOTHING")
                .bind(&id)
                .execute(&mut *tx)
                .await?;
            Ok((id, true))
        }
        None => {
            let id: String =
                sqlx::query_scalar("SELECT id FROM ferry.users WHERE telegram_user_id = $1")
                    .bind(telegram_user_id)
                    .fetch_one(&mut *tx)
                    .await?;
            Ok((id, false))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_ids_have_their_shapes() {
        let id = new_challenge_id();
        assert_eq!(id.len(), 4 + CHALLENGE_ID_RANDOM_LEN);
        assert!(id.starts_with("chl_"));
        assert!(id[4..].bytes().all(|b| LOWER_ALNUM.contains(&b)), "{id}");
        let code = new_start_code();
        assert_eq!(code.len(), START_CODE_LEN);
        assert!(is_start_code(&code), "{code}");
        let token = new_confirm_token();
        assert_eq!(token.len(), CONFIRM_TOKEN_LEN);
        assert!(token.bytes().all(|b| b.is_ascii_alphanumeric()), "{token}");
        assert_ne!(new_start_code(), code);
        assert_ne!(new_confirm_token(), token);
    }

    #[test]
    fn confirm_codes_are_four_digits_and_vary() {
        let codes: std::collections::HashSet<String> =
            (0..200).map(|_| new_confirm_code()).collect();
        for code in &codes {
            assert_eq!(code.len(), 4, "{code}");
            assert!(code.bytes().all(|b| b.is_ascii_digit()), "{code}");
        }
        // 200 draws from 10,000 codes: far more than 100 distinct ones, unless the generator is
        // stuck.
        assert!(codes.len() > 100, "{}", codes.len());
        // Leading zeros are kept: 1,000 draws all but surely include one.
        let zero = (0..1000)
            .map(|_| new_confirm_code())
            .any(|c| c.starts_with('0'));
        assert!(zero);
    }

    #[test]
    fn the_prompt_shows_the_code() {
        assert_eq!(
            prompt_text("4821"),
            "Sign in to Ferry on another device? Check that the screen shows 4821."
        );
        let sender = TelegramSender {
            id: "987654321",
            username: None,
            space_id: "987654321",
        };
        let sent = prompt(&sender, "0482", "tok");
        assert_eq!(sent.text, prompt_text("0482"));
        assert_eq!(
            sent.buttons,
            vec![
                Button::callback("Confirm", "signin:tok:confirm".into()),
                Button::callback("Not me", "signin:tok:cancel".into()),
            ]
        );
    }

    #[test]
    fn start_codes_fit_telegram() {
        assert!(is_start_code("abcdefghijklmnop"));
        assert!(is_start_code(&"A-_9".repeat(16)));
        for bad in [
            "",
            "short",
            "abcdefghijklmno",
            "abcdefghijklmnop!",
            "abcdefgh ijklmnop",
        ] {
            assert!(!is_start_code(bad), "{bad}");
        }
        assert!(!is_start_code(&"a".repeat(65)));
    }

    #[test]
    fn telegram_user_ids() {
        for good in ["1", "987654321", "7000000000"] {
            assert!(is_telegram_user_id(good), "{good}");
        }
        for bad in ["", "0", "0123", "-100123", "12a", "123456789012345678901"] {
            assert!(!is_telegram_user_id(bad), "{bad}");
        }
    }

    #[test]
    fn buttons_round_trip_under_64_bytes() {
        let token = new_confirm_token();
        for confirm in [true, false] {
            let data = button_data(&token, confirm);
            assert!(data.len() <= 64, "Telegram's callback_data limit: {data}");
            assert_eq!(parse_button(&data), Some((token.as_str(), confirm)));
        }
        for other in [
            "invite:inv_x:accept",
            "signin:abc",
            "signin:abc:confirm:x",
            "signin:abc:ok",
            "",
        ] {
            assert_eq!(parse_button(other), None, "{other}");
        }
    }

    #[test]
    fn colors_fit_the_schema_check() {
        for color in BUBBLE_COLORS {
            assert!(
                color.len() == 7
                    && color.starts_with('#')
                    && color[1..].bytes().all(|b| b.is_ascii_hexdigit()),
                "{color}"
            );
        }
    }
}
