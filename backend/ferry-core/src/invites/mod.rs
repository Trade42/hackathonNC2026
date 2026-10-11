//! Invites: /v1/invites, and the meetup invites `POST /v1/circles/{id}/meet-again` creates.
//!
//! Meet-again is a ping with a place and a time (Sal, 2026-10-11; contract v1.5): "Dana wants to
//! go to Cosmic Cantina at 7:00 PM. You in?", with "I'm in" and "Can't make it".
//!
//! - [`create_meetup_invite`] (the endpoint) and [`create_invite`] (the agent): one pending
//!   response per other member of the circle, a stored `invite` notification for each (titled with
//!   the ping's line, with `invite_id` and the `{web_url}/i/{invite_id}` deep link in its data),
//!   then `notification.created` to each invitee and `invite.updated` to every participant, and
//!   last a Telegram DM ([`InviteRecord::dms`]) to each invitee who pressed Start with the bot.
//! - The place ([`InvitePlace`]) is a restaurant or a known place by id, a map pin, or (from the
//!   agent only) a label someone typed, with no location. Times read as Chapel Hill's clock, as
//!   `run_open` reads one: `7:00 PM`, `8:00 PM tomorrow`, `7:00 PM on Sat, Oct 17`.
//! - `GET /v1/invites`: the invites the viewer sent or was invited to, newest first.
//! - `POST /v1/invites/{id}/respond {accept}`, or a DM button press ([`respond_from_button`]):
//!   invitees only. A second response by the same person overwrites the first. `invite.updated`
//!   goes to every participant after each response. An answer that changes the invitee's response
//!   is relayed to the inviter once: an `invite` notification ("Jordan's in." or "Maya can't make
//!   it.") written with the answer, and the same line as a DM when the inviter pressed Start.
//!
//! The state machine is [`invite_status`]: accepted once any invitee accepts, declined when every
//! invitee has declined, otherwise pending until the deadline (`proposed_time`, or [`OPEN_HOURS`]
//! after creation when no time was proposed) and expired after it. Expiry is computed on read.
//!
//! The group thread is a Telegram link (Sal's REQUEST-telegram.md; the bot cannot create groups).
//! `thread.open_url` is the circle's group invite link when the bot stored one
//! (`ferry.message_spaces.invite_url`), else the bot's `startgroup` link with the invite id as its
//! payload. It reaches only the inviter and the accepted invitees; every other copy (REST or event)
//! has `open_url: null`. `thread.state` is `ready` once an invitee has accepted and there is a link,
//! else `none`. `thread.compose` is always null: no phone numbers anywhere.

use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::PgPool;

use crate::agent::{dispatch, spaces};
use crate::catalog::types::{LatLng, Place};
use crate::circles::{self, PublicUser, iso};
use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path};
use crate::ids;
use crate::jwt::AuthUser;
use crate::messaging::mask::mask_space_id;
use crate::messaging::sidecar::{Button, SendRequest};
use crate::notifications::{Event, Notification, NotificationType, create_notification};
use crate::orders::pricing::{self, MAX_LABEL_CHARS};
use crate::profile::validate::FieldError;
use crate::state::AppState;

/// `GET /v1/invites` returns at most this many, newest first.
pub const LIST_LIMIT: i64 = 100;
/// The longest `message` meet-again accepts, in characters.
pub const MESSAGE_MAX_CHARS: usize = 280;
/// `message` when meet-again gets none (or only whitespace).
pub const DEFAULT_MESSAGE: &str = "Want to meet up again?";
/// How long an invite with no `proposed_time` stays open.
pub const OPEN_HOURS: i64 = 24;
/// How far ahead `proposed_time` may be, in days. It is also the latest deadline an invite can
/// have, so no invite stays pending for longer.
pub const MAX_PROPOSED_DAYS: i32 = 30;
/// The first part of a DM button's callback data: `invite:<invite_id>:accept|decline`.
pub const BUTTON_PREFIX: &str = "invite";
/// The DM's accept button.
pub const ACCEPT_LABEL: &str = "I'm in";
/// The DM's decline button.
pub const DECLINE_LABEL: &str = "Can't make it";

/// The SQL deadline of invite `i`: its proposed time, or `OPEN_HOURS` after it was created.
macro_rules! deadline {
    () => {
        "COALESCE(i.proposed_time, i.created_at + interval '24 hours')"
    };
}

/// Postgres formats proposed times for people in America/New_York (Chapel Hill), such as
/// `Sat, Oct 10 at 7:30 PM`.
macro_rules! local_time {
    ($e:literal) => {
        concat!(
            "to_char((",
            $e,
            ") AT TIME ZONE 'America/New_York', 'Dy, Mon FMDD \"at\" FMHH12:MI AM')"
        )
    };
}

/// A time as Chapel Hill reads a clock, the way `run_open` writes one (America/New_York):
/// `7:00 PM` today, `8:00 PM tomorrow`, otherwise `7:00 PM on Sat, Oct 17`. Null for null.
macro_rules! clock_time {
    ($e:literal) => {
        concat!(
            "CASE ((",
            $e,
            ") AT TIME ZONE 'America/New_York')::date \
                  - (now() AT TIME ZONE 'America/New_York')::date \
             WHEN 0 THEN to_char((",
            $e,
            ") AT TIME ZONE 'America/New_York', 'FMHH12:MI AM') \
             WHEN 1 THEN to_char((",
            $e,
            ") AT TIME ZONE 'America/New_York', 'FMHH12:MI AM') || ' tomorrow' \
             ELSE to_char((",
            $e,
            ") AT TIME ZONE 'America/New_York', 'FMHH12:MI AM \"on\" Dy, Mon FMDD') END"
        )
    };
}

/// The circle's latest stored Telegram group invite link, for invite `i`.
macro_rules! group_url {
    () => {
        "(SELECT s.invite_url FROM ferry.message_spaces s \
          WHERE s.circle_id = i.circle_id AND s.invite_url IS NOT NULL \
          ORDER BY s.created_at DESC, s.space_id LIMIT 1)"
    };
}

/// This module's routes, with full `/v1/...` paths. Merged into the app by `app::router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/invites", get(list))
        .route("/v1/invites/{id}/respond", post(respond))
}

/// `MeetAgainRequest` in contract/openapi.yaml. Every field is optional.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MeetAgainRequest {
    #[serde(default)]
    pub message: Option<String>,
    /// ISO 8601 (RFC 3339) with an offset, in the future.
    #[serde(default)]
    pub proposed_time: Option<String>,
    /// v1.5: a restaurant's or known place's id, or a label with a location.
    #[serde(default)]
    pub place: Option<Place>,
}

/// `InvitePlace` in contract/openapi.yaml (v1.5): where an invite proposes to go. A restaurant
/// (`rst_` id) or a known place (`place_` id) with its location, a map pin (no id), or a label
/// someone typed to the bot (no id, no location).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvitePlace {
    pub id: Option<String>,
    pub label: String,
    pub location: Option<LatLng>,
}

/// `RespondInviteRequest` in contract/openapi.yaml.
#[derive(Debug, Clone, Deserialize)]
pub struct RespondInviteRequest {
    pub accept: bool,
}

/// `Invite.kind` in contract/openapi.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InviteKind {
    Meetup,
}

/// `InviteStatus` in contract/openapi.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InviteStatus {
    Pending,
    Accepted,
    Declined,
    Expired,
}

impl InviteStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            InviteStatus::Pending => "pending",
            InviteStatus::Accepted => "accepted",
            InviteStatus::Declined => "declined",
            InviteStatus::Expired => "expired",
        }
    }
}

/// `InviteResponseStatus` in contract/openapi.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    Pending,
    Accepted,
    Declined,
}

impl ResponseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ResponseStatus::Pending => "pending",
            ResponseStatus::Accepted => "accepted",
            ResponseStatus::Declined => "declined",
        }
    }

    fn parse(value: &str) -> ApiResult<Self> {
        match value {
            "pending" => Ok(ResponseStatus::Pending),
            "accepted" => Ok(ResponseStatus::Accepted),
            "declined" => Ok(ResponseStatus::Declined),
            other => Err(ApiError::internal(format!(
                "unknown invite response status {other:?}"
            ))),
        }
    }
}

/// `InviteThreadState` in contract/openapi.yaml. `creating` is never used: the bot cannot create
/// groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadState {
    None,
    Creating,
    Ready,
}

impl ThreadState {
    pub fn as_str(self) -> &'static str {
        match self {
            ThreadState::None => "none",
            ThreadState::Creating => "creating",
            ThreadState::Ready => "ready",
        }
    }
}

/// `InviteResponse` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InviteResponse {
    pub user: PublicUser,
    pub status: ResponseStatus,
}

/// `ComposeMessage` in contract/openapi.yaml. Since Telegram replaced iMessage, `thread.compose`
/// is always null; the type stays for the contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComposeMessage {
    pub phones: Vec<String>,
    pub body: String,
}

/// `InviteThread` in contract/openapi.yaml (`open_url` from contract v1.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InviteThread {
    pub state: ThreadState,
    /// Always null.
    pub compose: Option<ComposeMessage>,
    /// A Telegram link to the circle's group, or the bot's startgroup link: only for the inviter
    /// and accepted invitees.
    pub open_url: Option<String>,
}

/// `Invite` in contract/openapi.yaml, as one viewer sees it ([`InviteRecord::as_seen_by`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Invite {
    pub id: String,
    pub kind: InviteKind,
    pub circle_id: String,
    pub from: PublicUser,
    pub message: String,
    /// ISO 8601 UTC with milliseconds.
    pub proposed_time: Option<String>,
    pub status: InviteStatus,
    pub responses: Vec<InviteResponse>,
    pub thread: InviteThread,
    /// v1.5: always sent, null without a place.
    pub place: Option<InvitePlace>,
}

/// The state machine: accepted once any invitee has accepted, declined when every invitee has
/// declined, otherwise pending until the deadline and expired after it. An accepted or declined
/// invite stays so after the deadline.
pub fn invite_status(responses: &[ResponseStatus], deadline_passed: bool) -> InviteStatus {
    if responses.contains(&ResponseStatus::Accepted) {
        InviteStatus::Accepted
    } else if !responses.is_empty() && responses.iter().all(|r| *r == ResponseStatus::Declined) {
        InviteStatus::Declined
    } else if deadline_passed {
        InviteStatus::Expired
    } else {
        InviteStatus::Pending
    }
}

/// The invite's Telegram link: the circle's group invite link when the bot stored one, else
/// `https://t.me/<bot>?startgroup=<invite_id>` (the bot learns which invite, and so which circle,
/// the new group is for), else none.
pub fn open_url(
    group_url: Option<&str>,
    bot_username: Option<&str>,
    invite_id: &str,
) -> Option<String> {
    group_url
        .map(str::to_owned)
        .or_else(|| bot_username.map(|bot| format!("https://t.me/{bot}?startgroup={invite_id}")))
}

/// One "I'm in" or "Can't make it" button on an invite DM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InviteButton {
    pub label: String,
    /// `invite:<invite_id>:accept` or `invite:<invite_id>:decline` (under Telegram's 64 bytes).
    pub callback_data: String,
}

/// The Telegram DM the bot sends one invitee about a new invite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InviteDm {
    /// The invitee (Ferry user id); the messaging bridge finds their Telegram chat.
    pub user_id: String,
    pub text: String,
    /// `{web_url}/i/{invite_id}` for a link button, when PUBLIC_WEB_URL is set.
    pub url: Option<String>,
    /// "I'm in", then "Can't make it".
    pub buttons: [InviteButton; 2],
}

impl InviteDm {
    /// This DM as the sidecar's `/send` to the invitee's Telegram chat: the two answer buttons,
    /// then the invite's link as an "Open in Ferry" button when it is https (Telegram refuses
    /// links to local addresses).
    pub fn into_send(self, chat: String) -> SendRequest {
        let mut buttons: Vec<Button> = self
            .buttons
            .into_iter()
            .map(|b| Button::callback(&b.label, b.callback_data))
            .collect();
        if let Some(url) = self.url.filter(|url| url.starts_with("https://")) {
            buttons.push(Button::url("Open in Ferry", &url));
        }
        SendRequest {
            space_id: chat,
            text: self.text,
            buttons,
        }
    }
}

/// The ping's first sentence, without its period: "Dana wants to go to Cosmic Cantina at 7:00 PM",
/// "Dana wants to go to Cosmic Cantina" (no time), "Dana wants to meet up at 7:00 PM" (no place)
/// or "Dana wants to meet up". `inviter` is a display name ("A friend" when empty).
pub fn ping_line(inviter: &str, place: Option<&str>, when: Option<&str>) -> String {
    let inviter = name_or_friend(inviter);
    match (place, when) {
        (Some(place), Some(when)) => format!("{inviter} wants to go to {place} at {when}"),
        (Some(place), None) => format!("{inviter} wants to go to {place}"),
        (None, Some(when)) => format!("{inviter} wants to meet up at {when}"),
        (None, None) => format!("{inviter} wants to meet up"),
    }
}

/// What an invite proposes, in a few words: "Cosmic Cantina at 7:00 PM", "Cosmic Cantina", "meeting
/// up at 7:00 PM", or None with neither a place nor a time.
pub fn plan(place: Option<&str>, when: Option<&str>) -> Option<String> {
    match (place, when) {
        (Some(place), Some(when)) => Some(format!("{place} at {when}")),
        (Some(place), None) => Some(place.to_owned()),
        (None, Some(when)) => Some(format!("meeting up at {when}")),
        (None, None) => None,
    }
}

/// What the inviter hears about one answer: "Jordan's in." or "Maya can't make it." `responder`
/// is the invitee's display name ("A friend" when empty).
pub fn relay_line(responder: &str, accept: bool) -> String {
    let responder = name_or_friend(responder);
    if accept {
        format!("{responder}'s in.")
    } else {
        format!("{responder} can't make it.")
    }
}

/// `text` with its first letter in capitals: "Meeting up at 7:00 PM".
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The callback data of an invite DM button.
pub fn button_data(invite_id: &str, accept: bool) -> String {
    let action = if accept { "accept" } else { "decline" };
    format!("{BUTTON_PREFIX}:{invite_id}:{action}")
}

/// Reads `invite:<invite_id>:accept|decline` back into the invite id and the answer.
pub fn parse_button(callback_data: &str) -> Option<(&str, bool)> {
    let mut parts = callback_data.split(':');
    let (prefix, invite_id, action) = (parts.next()?, parts.next()?, parts.next()?);
    if prefix != BUTTON_PREFIX || parts.next().is_some() || !invite_id.starts_with("inv_") {
        return None;
    }
    match action {
        "accept" => Some((invite_id, true)),
        "decline" => Some((invite_id, false)),
        _ => None,
    }
}

/// An invite with what every viewer's copy and the DMs need, loaded once and then shown to each
/// viewer through [`InviteRecord::as_seen_by`].
#[derive(Debug, Clone)]
pub struct InviteRecord {
    pub id: String,
    pub circle_id: String,
    pub circle_name: String,
    pub circle_emoji: String,
    pub from: PublicUser,
    pub message: String,
    pub proposed_time: Option<String>,
    /// `proposed_time` as Chapel Hill's clock ([`clock_time`]): `7:00 PM`, `8:00 PM tomorrow` or
    /// `7:00 PM on Sat, Oct 17`.
    pub when_local: Option<String>,
    pub place: Option<InvitePlace>,
    pub status: InviteStatus,
    pub responses: Vec<InviteResponse>,
    /// The invite's Telegram link ([`open_url`]), before per-viewer redaction.
    open_url: Option<String>,
    /// `{web_url}/i/{invite_id}`, when PUBLIC_WEB_URL is set.
    pub deep_link: Option<String>,
}

impl InviteRecord {
    /// The inviter, then each invitee: everyone who gets `invite.updated`.
    pub fn participants(&self) -> Vec<&str> {
        std::iter::once(self.from.id.as_str())
            .chain(self.responses.iter().map(|r| r.user.id.as_str()))
            .collect()
    }

    fn has_accepted(&self, user_id: &str) -> bool {
        self.responses
            .iter()
            .any(|r| r.user.id == user_id && r.status == ResponseStatus::Accepted)
    }

    /// The invite as `viewer` sees it. `thread.state` is `ready` for everyone once an invitee has
    /// accepted and there is a link; `thread.open_url` only for the inviter and accepted invitees.
    pub fn as_seen_by(&self, viewer: &str) -> Invite {
        let anyone_accepted = self
            .responses
            .iter()
            .any(|r| r.status == ResponseStatus::Accepted);
        let ready = anyone_accepted && self.open_url.is_some();
        let may_open = viewer == self.from.id || self.has_accepted(viewer);
        Invite {
            id: self.id.clone(),
            kind: InviteKind::Meetup,
            circle_id: self.circle_id.clone(),
            from: self.from.clone(),
            message: self.message.clone(),
            proposed_time: self.proposed_time.clone(),
            status: self.status,
            responses: self.responses.clone(),
            place: self.place.clone(),
            thread: InviteThread {
                state: if ready {
                    ThreadState::Ready
                } else {
                    ThreadState::None
                },
                compose: None,
                open_url: if ready && may_open {
                    self.open_url.clone()
                } else {
                    None
                },
            },
        }
    }

    fn place_label(&self) -> Option<&str> {
        self.place.as_ref().map(|p| p.label.as_str())
    }

    /// The ping's first sentence ([`ping_line`]): "Dana wants to go to Cosmic Cantina at 7:00 PM".
    pub fn ping_line(&self) -> String {
        ping_line(
            &self.from.display_name,
            self.place_label(),
            self.when_local.as_deref(),
        )
    }

    /// What the invite proposes ([`plan`]): "Cosmic Cantina at 7:00 PM".
    pub fn plan(&self) -> Option<String> {
        plan(self.place_label(), self.when_local.as_deref())
    }

    /// The inviter's own note, when they wrote one (not [`DEFAULT_MESSAGE`]).
    pub fn note(&self) -> Option<&str> {
        (self.message != DEFAULT_MESSAGE).then_some(self.message.as_str())
    }

    /// The Telegram DM for each invitee: "Dana wants to go to Cosmic Cantina at 7:00 PM. You in?",
    /// the inviter's note in quotes on a new line when there is one, and "I'm in" and "Can't make
    /// it" buttons whose presses go to [`respond_from_button`].
    pub fn dms(&self) -> Vec<InviteDm> {
        let mut text = format!("{}. You in?", self.ping_line());
        if let Some(note) = self.note() {
            text.push_str(&format!("\n“{note}”"));
        }
        self.responses
            .iter()
            .map(|r| InviteDm {
                user_id: r.user.id.clone(),
                text: text.clone(),
                url: self.deep_link.clone(),
                buttons: [
                    InviteButton {
                        label: ACCEPT_LABEL.into(),
                        callback_data: button_data(&self.id, true),
                    },
                    InviteButton {
                        label: DECLINE_LABEL.into(),
                        callback_data: button_data(&self.id, false),
                    },
                ],
            })
            .collect()
    }
}

/// A display name, or "A friend" for someone who has not set one.
fn name_or_friend(name: &str) -> &str {
    match name.trim() {
        "" => "A friend",
        name => name,
    }
}

/// Publishes `invite.updated` to every participant, each with their own copy. Call it after the
/// change commits. Returns how many connections it reached.
pub fn publish(state: &AppState, record: &InviteRecord) -> usize {
    state.hub.send_to_many(record.participants(), |viewer| {
        Event::invite_updated(&record.as_seen_by(viewer))
    })
}

#[derive(sqlx::FromRow)]
struct InviteRow {
    // `id` is the inviter's (`from`), so the invite's own id is `invite_id`.
    invite_id: String,
    circle_id: String,
    message: String,
    proposed_time: Option<String>,
    when_local: Option<String>,
    place_id: Option<String>,
    place_label: Option<String>,
    place_lat: Option<f64>,
    place_lng: Option<f64>,
    deadline_passed: bool,
    circle_name: String,
    circle_emoji: String,
    group_url: Option<String>,
    #[sqlx(flatten)]
    from: PublicUser,
}

/// The stored place columns as an [`InvitePlace`]: none without a label (the table's CHECKs keep
/// the parts together).
fn stored_place(
    id: Option<String>,
    label: Option<String>,
    lat: Option<f64>,
    lng: Option<f64>,
) -> Option<InvitePlace> {
    Some(InvitePlace {
        id,
        label: label?,
        location: lat.zip(lng).map(|(lat, lng)| LatLng { lat, lng }),
    })
}

#[derive(sqlx::FromRow)]
struct ResponseRow {
    invite_id: String,
    status: String,
    #[sqlx(flatten)]
    user: PublicUser,
}

/// The invites with these ids, newest first (ids that do not exist are skipped).
async fn load_records(state: &AppState, invite_ids: &[String]) -> ApiResult<Vec<InviteRecord>> {
    if invite_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, InviteRow>(concat!(
        "SELECT i.id AS invite_id, i.circle_id, i.message, ",
        iso!("i.proposed_time"),
        " AS proposed_time, ",
        clock_time!("i.proposed_time"),
        " AS when_local, i.place_id, i.place_label, i.place_lat, i.place_lng, now() >= ",
        deadline!(),
        " AS deadline_passed, c.name AS circle_name, c.emoji AS circle_emoji, ",
        group_url!(),
        " AS group_url, u.id, u.display_name, u.avatar_seed, u.bubble_color \
         FROM ferry.invites i \
         JOIN ferry.circles c ON c.id = i.circle_id \
         JOIN ferry.users u ON u.id = i.from_user_id \
         WHERE i.id = ANY($1) \
         ORDER BY i.created_at DESC, i.id DESC"
    ))
    .bind(invite_ids)
    .fetch_all(&state.pool)
    .await?;

    // Responses in the order the invitees joined the circle (anyone who has left it, last).
    let responses = sqlx::query_as::<_, ResponseRow>(
        "SELECT r.invite_id, r.status, u.id, u.display_name, u.avatar_seed, u.bubble_color \
         FROM ferry.invite_responses r \
         JOIN ferry.invites i ON i.id = r.invite_id \
         JOIN ferry.users u ON u.id = r.user_id \
         LEFT JOIN ferry.circle_members m ON m.circle_id = i.circle_id AND m.user_id = r.user_id \
         WHERE r.invite_id = ANY($1) \
         ORDER BY r.invite_id, m.joined_at NULLS LAST, u.id",
    )
    .bind(invite_ids)
    .fetch_all(&state.pool)
    .await?;

    let bot = state.config.telegram_bot_username.as_deref();
    let web = state.config.public_web_url.as_deref();
    rows.into_iter()
        .map(|row| {
            let responses = responses
                .iter()
                .filter(|r| r.invite_id == row.invite_id)
                .map(|r| {
                    Ok(InviteResponse {
                        user: r.user.clone(),
                        status: ResponseStatus::parse(&r.status)?,
                    })
                })
                .collect::<ApiResult<Vec<_>>>()?;
            let statuses: Vec<ResponseStatus> = responses.iter().map(|r| r.status).collect();
            Ok(InviteRecord {
                status: invite_status(&statuses, row.deadline_passed),
                open_url: open_url(row.group_url.as_deref(), bot, &row.invite_id),
                deep_link: web.map(|web| format!("{web}/i/{}", row.invite_id)),
                id: row.invite_id,
                circle_id: row.circle_id,
                circle_name: row.circle_name,
                circle_emoji: row.circle_emoji,
                from: row.from,
                message: row.message,
                proposed_time: row.proposed_time,
                when_local: row.when_local,
                place: stored_place(row.place_id, row.place_label, row.place_lat, row.place_lng),
                responses,
            })
        })
        .collect()
}

/// `at` (RFC 3339) on Chapel Hill's clock ([`clock_time`]): `7:00 PM`, `8:00 PM tomorrow` or `7:00
/// PM on Sat, Oct 17`, for a line about a time no invite stores yet.
pub async fn clock_words(pool: &PgPool, at: &str) -> ApiResult<String> {
    Ok(
        sqlx::query_scalar(concat!("SELECT ", clock_time!("$1::timestamptz")))
            .bind(at)
            .fetch_one(pool)
            .await?,
    )
}

/// One invite, or 404 `not_found`.
pub async fn load_record(state: &AppState, invite_id: &str) -> ApiResult<InviteRecord> {
    load_records(state, &[invite_id.to_owned()])
        .await?
        .pop()
        .ok_or_else(|| ApiError::not_found("No such invite."))
}

/// `message`, trimmed, or [`DEFAULT_MESSAGE`] when empty. Over [`MESSAGE_MAX_CHARS`] is 400.
fn clean_message(message: Option<String>) -> ApiResult<String> {
    let message = message.as_deref().map(str::trim).unwrap_or_default();
    if message.is_empty() {
        return Ok(DEFAULT_MESSAGE.to_owned());
    }
    if message.chars().count() > MESSAGE_MAX_CHARS {
        return Err(ApiError::validation(format!(
            "message is longer than {MESSAGE_MAX_CHARS} characters."
        )));
    }
    Ok(message.to_owned())
}

/// True when `s` has the shape of an RFC 3339 date-time with an offset, such as
/// `2026-10-11T23:30:00Z` or `2026-10-11T19:30:00.000-04:00`. Postgres then rejects impossible
/// dates.
fn is_rfc3339(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 20 {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| b[range].iter().all(u8::is_ascii_digit);
    let shape = digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && matches!(b[10], b'T' | b't')
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19);
    if !shape {
        return false;
    }
    let mut rest = &b[19..];
    if let Some(fraction) = rest.strip_prefix(b".") {
        let n = fraction.iter().take_while(|c| c.is_ascii_digit()).count();
        if n == 0 {
            return false;
        }
        rest = &fraction[n..];
    }
    match rest {
        [b'Z' | b'z'] => true,
        [sign, h1, h2, b':', m1, m2] => {
            matches!(sign, b'+' | b'-') && [h1, h2, m1, m2].iter().all(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

/// Checks `proposed_time` and returns its local wording for messages. 400 `validation_failed`
/// unless it is an RFC 3339 date-time in the future and at most [`MAX_PROPOSED_DAYS`] ahead.
async fn check_proposed_time(pool: &PgPool, proposed_time: &str) -> ApiResult<String> {
    let invalid = || {
        ApiError::validation(
            "proposed_time must be an ISO 8601 date-time with a time zone, such as \
             2026-10-11T23:30:00Z.",
        )
    };
    if !is_rfc3339(proposed_time) {
        return Err(invalid());
    }
    let (future, too_far, when): (bool, bool, String) = sqlx::query_as(concat!(
        "SELECT $1::timestamptz > now(), ",
        "$1::timestamptz > now() + make_interval(days => $2), ",
        local_time!("$1::timestamptz")
    ))
    .bind(proposed_time)
    .bind(MAX_PROPOSED_DAYS)
    .fetch_one(pool)
    .await
    .map_err(|err| match &err {
        // invalid_datetime_format, datetime_field_overflow (February 30, year 0) and
        // invalid_time_zone_displacement_value (an offset such as +16:00)
        sqlx::Error::Database(db)
            if matches!(db.code().as_deref(), Some("22007" | "22008" | "22009")) =>
        {
            invalid()
        }
        _ => err.into(),
    })?;
    if !future {
        return Err(ApiError::validation("proposed_time must be in the future."));
    }
    if too_far {
        return Err(ApiError::validation(format!(
            "proposed_time must be at most {MAX_PROPOSED_DAYS} days ahead."
        )));
    }
    Ok(when)
}

/// A 400 `validation_failed` naming `field`.
fn field_error(field: &str, message: impl Into<String>) -> ApiError {
    pricing::invalid(&[FieldError {
        field: field.to_owned(),
        message: message.into(),
    }])
}

/// The restaurant (`rst_` id) or known place (`place_` id, from `GET /v1/places`) with this id, as
/// an invite's place: its name or label and its location. None for any other id.
pub async fn catalog_place(pool: &PgPool, id: &str) -> ApiResult<Option<InvitePlace>> {
    let sql = if id.starts_with("rst_") {
        "SELECT name, lat, lng FROM ferry.restaurants WHERE id = $1"
    } else if id.starts_with("place_") {
        "SELECT label, lat, lng FROM ferry.places WHERE id = $1"
    } else {
        return Ok(None);
    };
    let row: Option<(String, f64, f64)> = sqlx::query_as(sql).bind(id).fetch_optional(pool).await?;
    Ok(row.map(|(label, lat, lng)| InvitePlace {
        id: Some(id.to_owned()),
        label,
        location: Some(LatLng { lat, lng }),
    }))
}

/// `MeetAgainRequest.place` as the invite keeps it. With an id: that restaurant's or known place's
/// name or label and location, whatever the request's label and location say. Without one: the
/// label and location, checked as a run's return place is (a label of 1 to 100 characters, valid
/// coordinates). 400 `validation_failed` otherwise.
async fn request_place(pool: &PgPool, place: &Place) -> ApiResult<InvitePlace> {
    if let Some(id) = place.id.as_deref() {
        return catalog_place(pool, id.trim()).await?.ok_or_else(|| {
            field_error(
                "place.id",
                format!("no restaurant or place has id {:?}", id.trim()),
            )
        });
    }
    let mut errors = Vec::new();
    let place = pricing::check_place(&mut errors, "place", place);
    if !errors.is_empty() {
        return Err(pricing::invalid(&errors));
    }
    Ok(InvitePlace {
        id: None,
        label: place.label,
        location: Some(place.location),
    })
}

/// `place` as it is stored: the label trimmed, 1 to [`MAX_LABEL_CHARS`] characters with no
/// control characters, valid coordinates, an id only of a restaurant or a known place. 400
/// `validation_failed` otherwise.
fn check_invite_place(place: InvitePlace) -> ApiResult<InvitePlace> {
    let label = place.label.trim();
    if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
        return Err(field_error(
            "place.label",
            format!("must be 1 to {MAX_LABEL_CHARS} characters"),
        ));
    }
    if label.chars().any(char::is_control) {
        return Err(field_error(
            "place.label",
            "must not contain control characters",
        ));
    }
    if place
        .location
        .is_some_and(|at| !crate::tracking::is_valid(at))
    {
        return Err(field_error(
            "place.location",
            "lat must be -90 to 90 and lng -180 to 180",
        ));
    }
    if place
        .id
        .as_deref()
        .is_some_and(|id| !id.starts_with("rst_") && !id.starts_with("place_"))
    {
        return Err(field_error(
            "place.id",
            "must be a restaurant's or a known place's id",
        ));
    }
    Ok(InvitePlace {
        label: label.to_owned(),
        ..place
    })
}

/// What a new invite says, already checked where it came from: the endpoint's
/// `MeetAgainRequest`, or the agent's reading of someone's words.
#[derive(Debug, Clone, Default)]
pub struct NewInvite {
    pub message: Option<String>,
    /// RFC 3339 with an offset, in the future.
    pub proposed_time: Option<String>,
    pub place: Option<InvitePlace>,
}

/// A new invite, and who it reached only in the app.
#[derive(Debug, Clone)]
pub struct Created {
    pub record: InviteRecord,
    /// Invitees with no Telegram DM (never pressed Start, or blocked the bot): they get the
    /// notification and its push only.
    pub app_only: Vec<String>,
}

/// `POST /v1/circles/{id}/meet-again`'s invite: [`create_invite`] with the request's place resolved
/// ([`request_place`]). Returns the record; the DMs are on their way.
pub async fn create_meetup_invite(
    state: &AppState,
    circle_id: &str,
    from_user_id: &str,
    request: MeetAgainRequest,
) -> ApiResult<InviteRecord> {
    let place = match &request.place {
        Some(place) => Some(request_place(&state.pool, place).await?),
        None => None,
    };
    let new = NewInvite {
        message: request.message,
        proposed_time: request.proposed_time,
        place,
    };
    Ok(create_invite(state, circle_id, from_user_id, new)
        .await?
        .record)
}

/// Creates a meetup invite from `from_user_id` to the other members of `circle_id`: one pending
/// response each, a stored `invite` notification each ("Dana wants to go to Cosmic Cantina at 7:00
/// PM", "You in?"), then `notification.created` to each invitee, `invite.updated` to every
/// participant, and each invitee's Telegram DM ([`InviteRecord::dms`]) on a task. 404 `not_found`
/// unless `from_user_id` is a member, 409 `conflict` when nobody else is in the circle or the
/// circle is a public community (an invite there would ping everyone in it), 400 `validation_failed` for a bad
/// message, proposed time or place.
pub async fn create_invite(
    state: &AppState,
    circle_id: &str,
    from_user_id: &str,
    new: NewInvite,
) -> ApiResult<Created> {
    if circles::is_public(&state.pool, circle_id).await? {
        return Err(ApiError::conflict(
            "That's a public community, so meet-ups go to one of your own circles.",
        ));
    }
    let message = clean_message(new.message)?;
    let proposed_time = new
        .proposed_time
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    if let Some(t) = &proposed_time {
        check_proposed_time(&state.pool, t).await?;
    }
    let place = new.place.map(check_invite_place).transpose()?;

    let mut tx = state.pool.begin().await?;
    let inviter: Option<PublicUser> = sqlx::query_as(
        "SELECT u.id, u.display_name, u.avatar_seed, u.bubble_color \
         FROM ferry.circle_members m JOIN ferry.users u ON u.id = m.user_id \
         WHERE m.circle_id = $1 AND m.user_id = $2",
    )
    .bind(circle_id)
    .bind(from_user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(inviter) = inviter else {
        return Err(ApiError::not_found("No such circle."));
    };

    let invite_id = ids::new_id(ids::INVITE);
    let when: Option<String> = sqlx::query_scalar(concat!(
        "INSERT INTO ferry.invites (id, circle_id, from_user_id, message, proposed_time, \
                                    place_id, place_label, place_lat, place_lng) \
         VALUES ($1, $2, $3, $4, $5::timestamptz, $6, $7, $8, $9) RETURNING ",
        clock_time!("proposed_time")
    ))
    .bind(&invite_id)
    .bind(circle_id)
    .bind(from_user_id)
    .bind(&message)
    .bind(proposed_time.as_deref())
    .bind(place.as_ref().and_then(|p| p.id.as_deref()))
    .bind(place.as_ref().map(|p| p.label.as_str()))
    .bind(place.as_ref().and_then(|p| p.location).map(|at| at.lat))
    .bind(place.as_ref().and_then(|p| p.location).map(|at| at.lng))
    .fetch_one(&mut *tx)
    .await?;
    let invitees: Vec<String> = sqlx::query_scalar(
        "INSERT INTO ferry.invite_responses (invite_id, user_id) \
         SELECT $1, m.user_id FROM ferry.circle_members m \
         WHERE m.circle_id = $2 AND m.user_id <> $3 \
         ORDER BY m.joined_at, m.user_id \
         RETURNING user_id",
    )
    .bind(&invite_id)
    .bind(circle_id)
    .bind(from_user_id)
    .fetch_all(&mut *tx)
    .await?;
    if invitees.is_empty() {
        return Err(ApiError::conflict(
            "Nobody else is in this circle yet, so there is no one to invite.",
        ));
    }

    let title = ping_line(
        &inviter.display_name,
        place.as_ref().map(|p| p.label.as_str()),
        when.as_deref(),
    );
    let body = if message == DEFAULT_MESSAGE {
        "You in?".to_owned()
    } else {
        format!("“{message}” You in?")
    };
    let mut data = Map::new();
    data.insert("invite_id".into(), Value::from(invite_id.as_str()));
    data.insert("circle_id".into(), Value::from(circle_id));
    if let Some(web) = &state.config.public_web_url {
        data.insert("url".into(), Value::from(format!("{web}/i/{invite_id}")));
    }
    let mut notifications: Vec<(String, Notification)> = Vec::with_capacity(invitees.len());
    for invitee in &invitees {
        let notification = create_notification(
            &mut *tx,
            invitee,
            NotificationType::Invite,
            &title,
            &body,
            Value::Object(data.clone()),
        )
        .await?;
        notifications.push((invitee.clone(), notification));
    }
    tx.commit().await?;
    tracing::info!(
        invite_id,
        circle_id,
        invitees = invitees.len(),
        place = place.is_some(),
        "meet-again invite created"
    );

    for (invitee, notification) in &notifications {
        state
            .hub
            .send_to(invitee, &Event::notification_created(notification));
    }
    let record = load_record(state, &invite_id).await?;
    publish(state, &record);
    let app_only = ping_invitees(state, &record).await;
    Ok(Created { record, app_only })
}

/// Sends each invitee who can get a Telegram DM their ping ([`InviteRecord::dms`]), in order on a
/// task. Returns the invitees who get the invite in the app only: no Telegram account, they
/// blocked the bot, or their chat could not be looked up.
async fn ping_invitees(state: &AppState, record: &InviteRecord) -> Vec<String> {
    let mut sends = Vec::new();
    let mut app_only = Vec::new();
    for dm in record.dms() {
        match spaces::reachable_dm(&state.pool, &dm.user_id).await {
            Ok(Some(chat)) => sends.push(dm.into_send(chat)),
            Ok(None) => app_only.push(dm.user_id),
            Err(err) => {
                tracing::warn!(invite_id = record.id, code = ?err.code, "an invitee's Telegram chat could not be read");
                app_only.push(dm.user_id);
            }
        }
    }
    dispatch::spawn_deliver(state, sends);
    app_only
}

/// The inviter's notification about one answer, written in the answer's transaction: title
/// "Jordan's in." or "Maya can't make it.", body what the invite proposes ("Cosmic Cantina at
/// 7:00 PM."), data `{invite_id, circle_id, url}`.
async fn relay_notification(
    conn: &mut sqlx::PgConnection,
    state: &AppState,
    invite_id: &str,
    inviter_id: &str,
    responder_id: &str,
    accept: bool,
) -> ApiResult<Notification> {
    let (responder, circle_id, place_label, when): (
        String,
        String,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(concat!(
        "SELECT u.display_name, i.circle_id, i.place_label, ",
        clock_time!("i.proposed_time"),
        " FROM ferry.invites i, ferry.users u WHERE i.id = $1 AND u.id = $2"
    ))
    .bind(invite_id)
    .bind(responder_id)
    .fetch_one(&mut *conn)
    .await?;
    let body = match plan(place_label.as_deref(), when.as_deref()) {
        Some(plan) => format!("{}.", capitalized(&plan)),
        None => "Your meetup invite.".to_owned(),
    };
    let mut data = Map::new();
    data.insert("invite_id".into(), Value::from(invite_id));
    data.insert("circle_id".into(), Value::from(circle_id));
    if let Some(web) = &state.config.public_web_url {
        data.insert("url".into(), Value::from(format!("{web}/i/{invite_id}")));
    }
    create_notification(
        &mut *conn,
        inviter_id,
        NotificationType::Invite,
        &relay_line(&responder, accept),
        &body,
        Value::Object(data),
    )
    .await
}

/// Records `user_id`'s answer to an invite and publishes `invite.updated` to every participant.
/// An answer that changes the invitee's response is relayed to the inviter once: an `invite`
/// notification written with the answer, and its title as a Telegram DM on a task when the inviter
/// can get one. 404 `not_found` unless the user is the inviter or an invitee, 403 `forbidden` for
/// the inviter, 409 `conflict` after the deadline. A second answer overwrites the first.
pub async fn respond_to_invite(
    state: &AppState,
    invite_id: &str,
    user_id: &str,
    accept: bool,
) -> ApiResult<InviteRecord> {
    let mut tx = state.pool.begin().await?;
    // The row lock serializes answers to one invite, so the stored status below is computed from
    // every answer committed before it, and the earlier answer read here is the latest one.
    let invite: Option<(String, bool, bool)> = sqlx::query_as(concat!(
        "SELECT i.from_user_id, now() >= ",
        deadline!(),
        ", ",
        group_url!(),
        " IS NOT NULL FROM ferry.invites i WHERE i.id = $1 FOR UPDATE OF i"
    ))
    .bind(invite_id)
    .fetch_optional(&mut *tx)
    .await?;
    let not_found = || ApiError::not_found("No such invite.");
    let Some((from_user_id, deadline_passed, has_group_url)) = invite else {
        return Err(not_found());
    };
    let earlier: Option<String> = sqlx::query_scalar(
        "SELECT status FROM ferry.invite_responses WHERE invite_id = $1 AND user_id = $2",
    )
    .bind(invite_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    if from_user_id == user_id {
        return Err(ApiError::forbidden(
            "You sent this invite; only the people you invited answer it.",
        ));
    }
    let Some(earlier) = earlier else {
        return Err(not_found());
    };
    if deadline_passed {
        return Err(ApiError::conflict("This invite has expired."));
    }

    let answer = if accept {
        ResponseStatus::Accepted
    } else {
        ResponseStatus::Declined
    };
    let statuses: Vec<String> = sqlx::query_scalar(
        "WITH answered AS ( \
             UPDATE ferry.invite_responses SET status = $3, responded_at = now() \
             WHERE invite_id = $1 AND user_id = $2 RETURNING user_id, status \
         ) \
         SELECT COALESCE(a.status, r.status) FROM ferry.invite_responses r \
         LEFT JOIN answered a ON a.user_id = r.user_id \
         WHERE r.invite_id = $1",
    )
    .bind(invite_id)
    .bind(user_id)
    .bind(answer.as_str())
    .fetch_all(&mut *tx)
    .await?;
    let statuses = statuses
        .iter()
        .map(|s| ResponseStatus::parse(s))
        .collect::<ApiResult<Vec<_>>>()?;
    // Stored for anyone reading the table; the API recomputes both on every read.
    let status = invite_status(&statuses, false);
    let link = has_group_url || state.config.telegram_bot_username.is_some();
    let thread = if status == InviteStatus::Accepted && link {
        ThreadState::Ready
    } else {
        ThreadState::None
    };
    sqlx::query("UPDATE ferry.invites SET status = $2, thread_state = $3 WHERE id = $1")
        .bind(invite_id)
        .bind(status.as_str())
        .bind(thread.as_str())
        .execute(&mut *tx)
        .await?;
    // Only a changed answer is news: the same answer again relays nothing.
    let relay = if earlier != answer.as_str() {
        Some(relay_notification(&mut tx, state, invite_id, &from_user_id, user_id, accept).await?)
    } else {
        None
    };
    tx.commit().await?;

    if let Some(notification) = &relay {
        state
            .hub
            .send_to(&from_user_id, &Event::notification_created(notification));
    }
    let record = load_record(state, invite_id).await?;
    publish(state, &record);
    if let Some(notification) = relay {
        relay_dm(state, &from_user_id, notification.title).await;
    }
    Ok(record)
}

/// Sends `line` to `inviter_id`'s Telegram DM on a task, when they can get one.
async fn relay_dm(state: &AppState, inviter_id: &str, line: String) {
    match spaces::reachable_dm(&state.pool, inviter_id).await {
        Ok(Some(chat)) => {
            tracing::info!(space = %mask_space_id(&chat), "invite answer relayed to the inviter");
            dispatch::spawn_deliver(
                state,
                vec![SendRequest {
                    space_id: chat,
                    text: line,
                    buttons: Vec::new(),
                }],
            );
        }
        Ok(None) => {}
        Err(err) => {
            tracing::warn!(code = ?err.code, "the inviter's Telegram chat could not be read")
        }
    }
}

/// A press of an invite DM's "I'm in" or "Can't make it" button (`callback_data` from [`button_data`]) by
/// `user_id`, the Ferry user behind the Telegram account that pressed it: the same rules and
/// events as `POST /v1/invites/{id}/respond`. 400 `validation_failed` for data that is not an
/// invite button's.
pub async fn respond_from_button(
    state: &AppState,
    user_id: &str,
    callback_data: &str,
) -> ApiResult<InviteRecord> {
    let (invite_id, accept) = parse_button(callback_data)
        .ok_or_else(|| ApiError::validation("That is not an invite button."))?;
    respond_to_invite(state, invite_id, user_id, accept).await
}

/// `GET /v1/invites`.
async fn list(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<Vec<Invite>>> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT i.id FROM ferry.invites i \
         JOIN ( \
             SELECT id FROM ferry.invites WHERE from_user_id = $1 \
             UNION SELECT invite_id FROM ferry.invite_responses WHERE user_id = $1 \
         ) mine ON mine.id = i.id \
         ORDER BY i.created_at DESC, i.id DESC LIMIT $2",
    )
    .bind(&user.user_id)
    .bind(LIST_LIMIT)
    .fetch_all(&state.pool)
    .await?;
    let records = load_records(&state, &ids).await?;
    Ok(Json(
        records
            .iter()
            .map(|record| record.as_seen_by(&user.user_id))
            .collect(),
    ))
}

/// `POST /v1/invites/{id}/respond`.
async fn respond(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(request): Json<RespondInviteRequest>,
) -> ApiResult<Json<Invite>> {
    let record = respond_to_invite(&state, &id, &user.user_id, request.accept).await?;
    Ok(Json(record.as_seen_by(&user.user_id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ResponseStatus::{Accepted, Declined, Pending};

    #[test]
    fn status_machine() {
        let cases: [(&[ResponseStatus], bool, InviteStatus); 12] = [
            (&[Pending, Pending], false, InviteStatus::Pending),
            (&[Pending, Pending], true, InviteStatus::Expired),
            (&[Accepted, Pending], false, InviteStatus::Accepted),
            (&[Accepted, Pending], true, InviteStatus::Accepted),
            (&[Declined, Accepted], false, InviteStatus::Accepted),
            (&[Declined, Pending], false, InviteStatus::Pending),
            (&[Declined, Pending], true, InviteStatus::Expired),
            (&[Declined, Declined], false, InviteStatus::Declined),
            (&[Declined, Declined], true, InviteStatus::Declined),
            (&[Declined], false, InviteStatus::Declined),
            (&[], false, InviteStatus::Pending),
            (&[], true, InviteStatus::Expired),
        ];
        for (responses, passed, expected) in cases {
            assert_eq!(
                invite_status(responses, passed),
                expected,
                "{responses:?} deadline passed {passed}"
            );
        }
    }

    fn enum_in_contract(schema: &str) -> Vec<String> {
        let yaml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contract/openapi.yaml"
        ))
        .unwrap();
        let header = format!("{schema}:");
        let line = yaml
            .lines()
            .skip_while(|l| l.trim() != header)
            .find(|l| l.trim_start().starts_with("enum:"))
            .unwrap();
        line.trim()
            .trim_start_matches("enum:")
            .trim()
            .trim_matches(['[', ']'])
            .split(',')
            .map(|s| s.trim().to_owned())
            .collect()
    }

    #[test]
    fn enums_match_the_contract() {
        let statuses = [
            InviteStatus::Pending,
            InviteStatus::Accepted,
            InviteStatus::Declined,
            InviteStatus::Expired,
        ];
        assert_eq!(
            enum_in_contract("InviteStatus"),
            statuses.map(|s| s.as_str())
        );
        for s in statuses {
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
        }
        let responses = [Pending, Accepted, Declined];
        assert_eq!(
            enum_in_contract("InviteResponseStatus"),
            responses.map(|s| s.as_str())
        );
        for s in responses {
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
            assert_eq!(ResponseStatus::parse(s.as_str()).unwrap(), s);
        }
        let threads = [ThreadState::None, ThreadState::Creating, ThreadState::Ready];
        assert_eq!(
            enum_in_contract("InviteThreadState"),
            threads.map(|s| s.as_str())
        );
        for s in threads {
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
        }
        assert_eq!(serde_json::to_value(InviteKind::Meetup).unwrap(), "meetup");
    }

    #[test]
    fn rfc3339_shapes() {
        for ok in [
            "2026-10-11T23:30:00Z",
            "2026-10-11T23:30:00.000Z",
            "2026-10-11t23:30:00z",
            "2026-10-11T19:30:00-04:00",
            "2026-10-11T19:30:00.123456+05:30",
        ] {
            assert!(is_rfc3339(ok), "{ok}");
        }
        for bad in [
            "",
            "tomorrow at 7",
            "2026-10-11",
            "2026-10-11 23:30:00Z",
            "2026-10-11T23:30Z",
            "2026-10-11T23:30:00",
            "2026-10-11T23:30:00.Z",
            "2026-10-11T23:30:00+0400",
            "2026-10-11T23:30:00+04",
            "2026-10-11T23:30:00Z ",
            "２０２６-10-11T23:30:00Z",
        ] {
            assert!(!is_rfc3339(bad), "{bad}");
        }
    }

    #[test]
    fn messages() {
        assert_eq!(clean_message(None).unwrap(), DEFAULT_MESSAGE);
        assert_eq!(clean_message(Some("  ".into())).unwrap(), DEFAULT_MESSAGE);
        assert_eq!(clean_message(Some(" Tacos? ".into())).unwrap(), "Tacos?");
        let longest = "é".repeat(MESSAGE_MAX_CHARS);
        assert_eq!(clean_message(Some(longest.clone())).unwrap(), longest);
        let err = clean_message(Some(format!("{longest}x"))).unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::ValidationFailed);
    }

    #[test]
    fn the_sql_deadline_uses_open_hours() {
        assert!(deadline!().contains(&format!("interval '{OPEN_HOURS} hours'")));
    }

    fn user(id: &str) -> PublicUser {
        PublicUser {
            id: id.into(),
            display_name: id.trim_start_matches("usr_").into(),
            avatar_seed: id.into(),
            bubble_color: "#336699".into(),
        }
    }

    fn record(statuses: [ResponseStatus; 3], open: Option<&str>) -> InviteRecord {
        InviteRecord {
            id: "inv_test".into(),
            circle_id: "cir_hinton4".into(),
            circle_name: "Hinton James 4th floor".into(),
            circle_emoji: "🌊".into(),
            from: user("usr_dana"),
            message: DEFAULT_MESSAGE.into(),
            proposed_time: None,
            when_local: None,
            place: None,
            status: invite_status(&statuses, false),
            responses: ["usr_judge", "usr_maya", "usr_leo"]
                .iter()
                .zip(statuses)
                .map(|(id, status)| InviteResponse {
                    user: user(id),
                    status,
                })
                .collect(),
            open_url: open.map(str::to_owned),
            deep_link: Some("https://ferry.example/i/inv_test".into()),
        }
    }

    #[test]
    fn open_url_only_for_the_inviter_and_accepted_invitees() {
        let link = "https://t.me/Ferrygo_bot?startgroup=inv_test";
        let record = record([Accepted, Pending, Declined], Some(link));
        assert_eq!(
            record.participants(),
            ["usr_dana", "usr_judge", "usr_maya", "usr_leo"]
        );
        for viewer in ["usr_dana", "usr_judge"] {
            let thread = record.as_seen_by(viewer).thread;
            assert_eq!(thread.state, ThreadState::Ready, "{viewer}");
            assert_eq!(thread.open_url.as_deref(), Some(link), "{viewer}");
            assert_eq!(thread.compose, None, "{viewer}");
        }
        for viewer in ["usr_maya", "usr_leo", "usr_outsider"] {
            let thread = record.as_seen_by(viewer).thread;
            assert_eq!(thread.state, ThreadState::Ready, "{viewer}");
            assert_eq!(thread.open_url, None, "{viewer}");
            assert_eq!(thread.compose, None, "{viewer}");
        }
        let json = serde_json::to_value(record.as_seen_by("usr_maya")).unwrap();
        assert_eq!(
            json["thread"],
            serde_json::json!({"state": "ready", "compose": null, "open_url": null})
        );
    }

    #[test]
    fn no_thread_until_someone_accepts_and_there_is_a_link() {
        let link = Some("https://t.me/+AbCdEf123");
        for statuses in [[Pending; 3], [Declined, Pending, Declined], [Declined; 3]] {
            let record = record(statuses, link);
            for viewer in record.participants() {
                let thread = record.as_seen_by(viewer).thread;
                assert_eq!(thread.state, ThreadState::None);
                assert_eq!(thread.open_url, None);
            }
        }
        // Accepted, but no group link and no bot username: no link to give.
        let record = record([Accepted; 3], None);
        for viewer in record.participants() {
            let thread = record.as_seen_by(viewer).thread;
            assert_eq!(thread.state, ThreadState::None);
            assert_eq!(thread.open_url, None);
        }
    }

    #[test]
    fn the_group_link_wins_over_the_startgroup_link() {
        assert_eq!(
            open_url(
                Some("https://t.me/+AbCdEf123"),
                Some("Ferrygo_bot"),
                "inv_x"
            )
            .as_deref(),
            Some("https://t.me/+AbCdEf123")
        );
        assert_eq!(
            open_url(None, Some("Ferrygo_bot"), "inv_x").as_deref(),
            Some("https://t.me/Ferrygo_bot?startgroup=inv_x")
        );
        assert_eq!(open_url(None, None, "inv_x"), None);
    }

    #[test]
    fn buttons_round_trip() {
        let id = ids::new_id(ids::INVITE);
        for accept in [true, false] {
            let data = button_data(&id, accept);
            assert!(data.len() <= 64, "Telegram's callback_data limit: {data}");
            assert_eq!(parse_button(&data), Some((id.as_str(), accept)));
        }
        assert_eq!(button_data("inv_x", true), "invite:inv_x:accept");
        assert_eq!(button_data("inv_x", false), "invite:inv_x:decline");
        for bad in [
            "",
            "invite",
            "invite:inv_x",
            "invite:inv_x:maybe",
            "invite:inv_x:accept:extra",
            "invite:run_x:accept",
            "order:inv_x:accept",
            "invite::accept",
        ] {
            assert_eq!(parse_button(bad), None, "{bad}");
        }
    }

    #[test]
    fn one_dm_per_invitee() {
        let mut record = record([Pending; 3], None);
        record.place = Some(InvitePlace {
            id: Some("rst_cosmic".into()),
            label: "Cosmic Cantina".into(),
            location: Some(LatLng {
                lat: 35.9105,
                lng: -79.0618,
            }),
        });
        record.when_local = Some("7:00 PM".into());
        let dms = record.dms();
        let to: Vec<&str> = dms.iter().map(|d| d.user_id.as_str()).collect();
        assert_eq!(to, ["usr_judge", "usr_maya", "usr_leo"]);
        for dm in &dms {
            assert_eq!(
                dm.text,
                "dana wants to go to Cosmic Cantina at 7:00 PM. You in?"
            );
            assert_eq!(dm.url.as_deref(), Some("https://ferry.example/i/inv_test"));
            assert_eq!(dm.buttons[0].label, "I'm in");
            assert_eq!(dm.buttons[0].callback_data, "invite:inv_test:accept");
            assert_eq!(dm.buttons[1].label, "Can't make it");
            assert_eq!(dm.buttons[1].callback_data, "invite:inv_test:decline");
        }
        let send = dms[0].clone().into_send("1001".into());
        assert_eq!(send.space_id, "1001");
        assert_eq!(
            serde_json::to_value(&send.buttons).unwrap(),
            serde_json::json!([
                {"label": "I'm in", "callback_data": "invite:inv_test:accept"},
                {"label": "Can't make it", "callback_data": "invite:inv_test:decline"},
                {"label": "Open in Ferry", "url": "https://ferry.example/i/inv_test"},
            ])
        );
    }

    #[test]
    fn the_ping_with_a_place_a_time_one_of_them_or_neither() {
        let mut record = record([Pending; 3], None);
        let cosmic = InvitePlace {
            id: None,
            label: "Cosmic Cantina".into(),
            location: None,
        };
        let cases = [
            (
                Some(cosmic.clone()),
                Some("7:00 PM"),
                "dana wants to go to Cosmic Cantina at 7:00 PM. You in?",
            ),
            (
                Some(cosmic),
                None,
                "dana wants to go to Cosmic Cantina. You in?",
            ),
            (
                None,
                Some("8:00 PM tomorrow"),
                "dana wants to meet up at 8:00 PM tomorrow. You in?",
            ),
            (None, None, "dana wants to meet up. You in?"),
        ];
        for (place, when, text) in cases {
            record.place = place;
            record.when_local = when.map(str::to_owned);
            assert_eq!(record.dms()[0].text, text);
        }
        // The inviter's own note goes under it; a missing name reads "A friend".
        record.message = "Bring cash".into();
        record.from.display_name = " ".into();
        assert_eq!(
            record.dms()[0].text,
            "A friend wants to meet up. You in?\n“Bring cash”"
        );
    }

    #[test]
    fn relay_lines_and_plans() {
        assert_eq!(relay_line("Jordan", true), "Jordan's in.");
        assert_eq!(relay_line("Maya", false), "Maya can't make it.");
        assert_eq!(relay_line("", true), "A friend's in.");
        assert_eq!(
            plan(Some("Sup Dogs"), Some("8:00 PM tomorrow")).as_deref(),
            Some("Sup Dogs at 8:00 PM tomorrow")
        );
        assert_eq!(plan(Some("Sup Dogs"), None).as_deref(), Some("Sup Dogs"));
        assert_eq!(
            plan(None, Some("7:00 PM")).as_deref(),
            Some("meeting up at 7:00 PM")
        );
        assert_eq!(plan(None, None), None);
        assert_eq!(
            capitalized("meeting up at 7:00 PM"),
            "Meeting up at 7:00 PM"
        );
    }

    #[test]
    fn stored_places_and_their_checks() {
        assert_eq!(stored_place(None, None, None, None), None);
        assert_eq!(
            stored_place(None, Some("Maya's room".into()), None, None),
            Some(InvitePlace {
                id: None,
                label: "Maya's room".into(),
                location: None
            })
        );
        let ok = |label: &str, id: Option<&str>, at: Option<(f64, f64)>| {
            check_invite_place(InvitePlace {
                id: id.map(str::to_owned),
                label: label.into(),
                location: at.map(|(lat, lng)| LatLng { lat, lng }),
            })
        };
        assert_eq!(
            ok("  Cosmic  ", Some("rst_cosmic"), None).unwrap().label,
            "Cosmic"
        );
        assert!(ok("Fetzer", Some("place_fetzer"), Some((35.9, -79.0))).is_ok());
        assert!(ok(&"x".repeat(MAX_LABEL_CHARS), None, None).is_ok());
        for (label, id, at) in [
            ("", None, None),
            ("   ", None, None),
            ("a\nb", None, None),
            ("Cosmic", Some("usr_maya"), None),
            ("Cosmic", None, Some((91.0, 0.0))),
        ] {
            let err = ok(label, id, at).unwrap_err();
            assert_eq!(
                err.code,
                crate::error::ErrorCode::ValidationFailed,
                "{label:?}"
            );
        }
        let long = "x".repeat(MAX_LABEL_CHARS + 1);
        assert!(ok(&long, None, None).is_err());
    }
}
