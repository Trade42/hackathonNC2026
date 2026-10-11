//! Who the bot answers, and when (spec section 3, adapted to Telegram).
//!
//! [`dispatch`] gets every Telegram event that is not sign-in and decides at once, with a few
//! quick reads, whether the bot speaks. The sidecar is answered `{replies: []}` right away; the
//! turn runs on its own task and sends through the sidecar's `POST /send`, so a slow Gemini never
//! holds the sidecar past its 3 s timeout (which would make it deliver the event again).
//!
//! - **Groups:** the bot speaks only when addressed ("ferry", "@<bot>", a reply to the bot when
//!   the sidecar says so), when a trip is announced, or for its own buttons. One message per
//!   event. Every other message is silence: no Gemini call and no log.
//! - **DMs:** every message is answered.
//! - **Unknown senders** (no Ferry user for the Telegram id) get one line with the app's sign-in
//!   link, then silence ([`ferry.unknown_senders`]).
//! - **Never twice:** each event is claimed in `agent_turns` by its key before the turn starts;
//!   a second delivery finds the claim and stays silent.
//! - Meet-again's "I'm in" and "Can't make it" (`invite:<id>:accept|decline`) answer through
//!   invites' respond, under the same rules as the app, which relays the answer to the inviter.
//!   The fallback's meet-again question's Send and Cancel (`meet:<id>:send|cancel`) go to
//!   [`meet`]. The trip question's Yes and No go to [`trips`].
//!   A draft's Place order and Cancel, and a placed order's Cancel, go to [`ordering`]: the only
//!   ways an order is placed or cancelled from Telegram.
//! - **A turn's failure is answered:** the turn runs on a task of its own ([`run_caught`]), so a
//!   panic is caught like an error; in a DM the user gets [`SNAG`] (never after a Place order
//!   press, whose own task says what happened), and the turn's row is finished either way.
//!
//! Phase B adds run announcements here: a run the agent manages changing state gets one
//! message in its group.

use std::future::Future;
use std::time::{Duration, Instant};

use super::log::{self, Claim, Outcome};
use super::meet;
use super::ordering;
use super::patterns;
use super::spaces;
use super::tools;
use super::trips;
use super::turn::{self, Turn, elapsed_ms};
use crate::catalog::public_config::DEFAULT_WEB_URL;
use crate::error::{ApiError, ApiResult, ErrorCode};
use crate::invites;
use crate::messaging::inbound::{InboundEvent, start_payload};
use crate::messaging::mask::{mask_space_id, mask_telegram_id};
use crate::messaging::sidecar::{self, Button, SendRequest};
use crate::state::AppState;

/// The longest a spawned turn may run, Gemini, tools and sends included.
pub const WORK_TIMEOUT: Duration = Duration::from_secs(45);

/// What an unknown sender hears, once.
pub const UNKNOWN_SENDER: &str =
    "I don't know you yet. Sign in to Ferry with Telegram in the app, then message me again.";
/// What the bot says when a turn fails in a DM (in a group it stays silent).
pub const SNAG: &str = "Ferry hit a snag answering that. Try again in a moment.";

/// One Telegram event from the sidecar, read: `messaging::inbound` has checked the channel and
/// the sender's id already.
pub struct Event<'a> {
    pub raw: &'a InboundEvent,
    pub kind: &'a str,
    pub space_id: &'a str,
    pub sender_id: &'a str,
}

impl Event<'_> {
    fn text(&self) -> &str {
        self.raw.text.as_deref().unwrap_or("").trim()
    }

    /// The key that makes this event's turn unique ([`log::inbound_key`]).
    fn key(&self) -> String {
        log::inbound_key(&[
            self.raw.channel.as_deref().unwrap_or(""),
            self.kind,
            self.space_id,
            self.sender_id,
            self.raw.received_at.as_deref().unwrap_or(""),
            self.raw.text.as_deref().unwrap_or(""),
            self.raw.callback_data.as_deref().unwrap_or(""),
        ])
    }
}

/// Handles `event`. Everything the bot says goes out through the sidecar's `/send`, on a task.
pub async fn dispatch(state: &AppState, event: &Event<'_>) -> ApiResult<()> {
    super::announce::ensure_started(state);
    match event.kind {
        "membership" => {
            let membership = spaces::Membership {
                space_id: event.space_id,
                is_group: event.raw.is_group,
                sender_id: event.sender_id,
                chat_title: event.raw.chat_title.as_deref(),
                bot_in_chat: event.raw.bot_in_chat.unwrap_or(true),
                bot_is_admin: event.raw.bot_is_admin,
                migrated_from_space_id: event.raw.migrated_from_space_id.as_deref(),
            };
            let sends = spaces::on_membership(state, &membership).await?;
            spawn_deliver(state, sends);
            Ok(())
        }
        "callback" => on_callback(state, event).await,
        "message" => on_message(state, event).await,
        _ => Ok(()),
    }
}

/// What a turn does once claimed.
enum Work {
    /// Answer the text (DM, or the bot was addressed).
    Answer,
    /// A trip announcement, with the prefilter's reading. `addressed` (a DM, or the bot was
    /// addressed) means the message still gets an answer when it turns out not to be a trip.
    Trip {
        mention: patterns::TripMention,
        addressed: bool,
    },
    /// A press of a meet-again invite's "I'm in" or "Can't make it".
    InviteButton,
    /// A press of the fallback's meet-again question's Send or Cancel.
    MeetButton,
    /// A press of a trip question's Yes or No.
    TripButton,
    /// An order in words on an open run ("chicken burrito no beans"): draft it.
    Order(ordering::OpenRun),
    /// An edit of the user's pending draft in this chat ("add guac").
    OrderEdit(ordering::Draft),
    /// A press of a draft's Place order or Cancel.
    OrderButton,
}

async fn on_message(state: &AppState, event: &Event<'_>) -> ApiResult<()> {
    let text = event.text();
    if text.is_empty() {
        return Ok(());
    }
    let user = spaces::user_for_telegram(&state.pool, event.sender_id).await?;
    let circle_id = if event.raw.is_group {
        let circle_id =
            spaces::ensure_group(state, event.space_id, event.raw.chat_title.as_deref()).await?;
        if let Some(user_id) = &user {
            spaces::add_member(&state.pool, &circle_id, user_id).await?;
        }
        // `/start@<bot> <payload>` from a startgroup link.
        if let Some(payload) = start_payload(text) {
            if let (Some(user_id), Some(payload)) = (&user, payload) {
                let sends = spaces::on_startgroup(state, event.space_id, user_id, payload).await?;
                spawn_deliver(state, sends);
            }
            return Ok(());
        }
        Some(circle_id)
    } else {
        None
    };

    let trip = patterns::prefilter(text);
    let addressed = !event.raw.is_group
        || event.raw.reply_to_bot == Some(true)
        || patterns::addressed(text, state.config.telegram_bot_username.as_deref());
    let Some(user_id) = user else {
        // An unknown sender ordering on a group's open run is told to sign in too.
        let ordering = trip.is_none()
            && !addressed
            && ordering::run_for_text(&state.pool, "", circle_id.as_deref(), text)
                .await?
                .is_some();
        if trip.is_some() || addressed || ordering {
            return nag_once(state, event).await;
        }
        return Ok(());
    };
    if !event.raw.is_group {
        spaces::ensure_dm(&state.pool, event.space_id, &user_id).await?;
    }
    let work = match trip {
        Some(mention) => Work::Trip { mention, addressed },
        None => match order_work(state, event, &user_id, circle_id.as_deref(), text).await? {
            Some(work) => work,
            None if addressed => Work::Answer,
            // A group message that is none of the bot's business: silence.
            None => return Ok(()),
        },
    };
    claim_and_spawn(state, event, user_id, circle_id, work).await
}

/// Whether `text` edits the user's pending draft in this chat, or (in a group not addressing the
/// bot) orders on an open run of the group's circle. In a DM a new order goes through the turn,
/// so Gemini or the fallback can ask which run.
async fn order_work(
    state: &AppState,
    event: &Event<'_>,
    user_id: &str,
    circle_id: Option<&str>,
    text: &str,
) -> ApiResult<Option<Work>> {
    if let Some(draft) = ordering::pending_draft(&state.pool, user_id, event.space_id).await?
        && ordering::is_edit(state, &draft, text).await?
    {
        return Ok(Some(Work::OrderEdit(draft)));
    }
    let addressed = patterns::addressed(text, state.config.telegram_bot_username.as_deref());
    if event.raw.is_group
        && !addressed
        && let Some(run) = ordering::run_for_text(&state.pool, user_id, circle_id, text).await?
    {
        return Ok(Some(Work::Order(run)));
    }
    Ok(None)
}

async fn on_callback(state: &AppState, event: &Event<'_>) -> ApiResult<()> {
    let data = event.raw.callback_data.as_deref().unwrap_or("");
    let work = if invites::parse_button(data).is_some() {
        Work::InviteButton
    } else if meet::parse_button(data).is_some() {
        Work::MeetButton
    } else if trips::parse_button(data).is_some() {
        Work::TripButton
    } else if ordering::parse_button(data).is_some()
        || ordering::parse_cancel_button(data).is_some()
    {
        Work::OrderButton
    } else {
        return Ok(());
    };
    let Some(user_id) = spaces::user_for_telegram(&state.pool, event.sender_id).await? else {
        return nag_once(state, event).await;
    };
    claim_and_spawn(state, event, user_id, None, work).await
}

/// One line with the app's link to a Telegram account Ferry does not know, the first time only.
async fn nag_once(state: &AppState, event: &Event<'_>) -> ApiResult<()> {
    let first: Option<String> = sqlx::query_scalar(
        "INSERT INTO ferry.unknown_senders (telegram_user_id) VALUES ($1)
         ON CONFLICT DO NOTHING RETURNING telegram_user_id",
    )
    .bind(event.sender_id)
    .fetch_optional(&state.pool)
    .await?;
    if first.is_none() {
        return Ok(());
    }
    tracing::info!(tg = %mask_telegram_id(event.sender_id), "unknown Telegram sender told to sign in");
    spawn_deliver(
        state,
        vec![app_link_line(state, event.space_id, UNKNOWN_SENDER)],
    );
    Ok(())
}

/// `text` with the web app's link: a button when the URL is https, else the URL at the end
/// (Telegram refuses link buttons to local addresses).
pub fn app_link_line(state: &AppState, space_id: &str, text: &str) -> SendRequest {
    let web = state
        .config
        .public_web_url
        .as_deref()
        .unwrap_or(DEFAULT_WEB_URL);
    if web.starts_with("https://") {
        SendRequest {
            space_id: space_id.to_owned(),
            text: text.to_owned(),
            buttons: vec![Button::url("Open Ferry", web)],
        }
    } else {
        SendRequest {
            space_id: space_id.to_owned(),
            text: format!("{text} {web}"),
            buttons: Vec::new(),
        }
    }
}

/// Claims the event in `agent_turns` and runs `work` on its own task. A second delivery of the
/// same event finds the claim and stops here.
async fn claim_and_spawn(
    state: &AppState,
    event: &Event<'_>,
    user_id: String,
    circle_id: Option<String>,
    work: Work,
) -> ApiResult<()> {
    let key = event.key();
    let inbound_text = match event.kind {
        "callback" => event.raw.callback_data.clone().unwrap_or_default(),
        _ => event.text().to_owned(),
    };
    let claim = Claim {
        inbound_key: &key,
        space_id: event.space_id,
        user_id: Some(&user_id),
        is_group: event.raw.is_group,
        inbound_text: &inbound_text,
    };
    let Some(turn_id) = log::claim(&state.pool, &claim).await? else {
        tracing::info!(space = %mask_space_id(event.space_id), "event already answered; staying silent");
        return Ok(());
    };
    let job = Job {
        state: state.clone(),
        turn_id,
        user_id,
        space_id: event.space_id.to_owned(),
        is_group: event.raw.is_group,
        circle_id,
        text: inbound_text,
        sender_name: event.raw.sender_name.clone(),
        sender_username: event.raw.sender_username.clone(),
        work,
    };
    tokio::spawn(job.run());
    Ok(())
}

/// A claimed turn, run on its own task.
struct Job {
    state: AppState,
    turn_id: i64,
    user_id: String,
    space_id: String,
    is_group: bool,
    circle_id: Option<String>,
    /// The message text, or a button's callback data.
    text: String,
    sender_name: Option<String>,
    sender_username: Option<String>,
    work: Work,
}

/// Runs `work` on a task of its own, so a panic in it comes back as Err("panic") instead of
/// ending the caller's task; after `limit` the task is stopped and Err("work_timeout") returned.
/// Tasks `work` spawned itself (a Place order press's) run on.
pub async fn run_caught<T: Send + 'static>(
    work: impl Future<Output = T> + Send + 'static,
    limit: Duration,
) -> Result<T, &'static str> {
    let task = tokio::spawn(work);
    let abort = task.abort_handle();
    match tokio::time::timeout(limit, task).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err("panic"),
        Err(_) => {
            abort.abort();
            Err("work_timeout")
        }
    }
}

impl Job {
    async fn run(self) {
        let started = Instant::now();
        let (state, turn_id, space_id, is_group) = (
            self.state.clone(),
            self.turn_id,
            self.space_id.clone(),
            self.is_group,
        );
        // A Place order press's own task tells the user what happened; a snag line here could
        // say "try again" about an order that went through.
        let snag_allowed = !matches!(self.work, Work::OrderButton);
        let mut outcome = match run_caught(self.work(), WORK_TIMEOUT).await {
            Ok(Ok((sends, outcome))) => {
                deliver(&state, &sends).await;
                outcome
            }
            Ok(Err(err)) => {
                tracing::warn!(turn = turn_id, code = ?err.code, "agent turn failed");
                Outcome {
                    error: Some(format!("{:?}", err.code)),
                    ..Outcome::default()
                }
            }
            Err(reason) => {
                tracing::warn!(turn = turn_id, reason, "agent turn did not finish");
                Outcome {
                    error: Some(reason.to_owned()),
                    ..Outcome::default()
                }
            }
        };
        if outcome.reply_text.is_none() && outcome.error.is_some() && !is_group && snag_allowed {
            let snag = SendRequest {
                space_id: space_id.clone(),
                text: SNAG.to_owned(),
                buttons: Vec::new(),
            };
            deliver(&state, std::slice::from_ref(&snag)).await;
            outcome.reply_text = Some(SNAG.to_owned());
        }
        log::finish(&state.pool, turn_id, &outcome, elapsed_ms(started)).await;
    }

    /// The messages to send (the reply first) and the log's outcome.
    async fn work(self) -> ApiResult<(Vec<SendRequest>, Outcome)> {
        let state = &self.state;
        let mut sends = Vec::new();
        let mut turn = match self.work {
            Work::InviteButton => {
                let reply = invite_button(state, &self.space_id, &self.user_id, &self.text).await?;
                let outcome = Outcome {
                    reply_text: Some(reply.text.clone()),
                    ..Outcome::default()
                };
                return Ok((vec![reply], outcome));
            }
            Work::MeetButton => {
                let reply =
                    meet::on_button(state, &self.space_id, &self.user_id, &self.text).await?;
                let outcome = Outcome {
                    reply_text: Some(reply.text.clone()),
                    ..Outcome::default()
                };
                return Ok((vec![reply], outcome));
            }
            Work::TripButton => {
                let replies =
                    trips::on_button(state, &self.space_id, &self.user_id, &self.text).await?;
                let outcome = Outcome {
                    reply_text: joined(&replies),
                    ..Outcome::default()
                };
                return Ok((replies, outcome));
            }
            Work::OrderButton => {
                let (replies, said) = ordering::on_button(
                    state,
                    &self.space_id,
                    &self.user_id,
                    &self.text,
                    self.is_group,
                )
                .await?;
                let outcome = Outcome {
                    reply_text: said.or_else(|| joined(&replies)),
                    ..Outcome::default()
                };
                return Ok((replies, outcome));
            }
            Work::Order(ref run) => {
                let reply = ordering::draft_from_text(
                    state,
                    &self.user_id,
                    &self.space_id,
                    self.is_group,
                    run,
                    &self.text,
                )
                .await?;
                let send = self.addressed_to_drafter(reply).await?;
                let outcome = Outcome {
                    reply_text: Some(send.text.clone()),
                    ..Outcome::default()
                };
                return Ok((vec![send], outcome));
            }
            Work::OrderEdit(ref draft) => {
                let reply = ordering::edit_draft(
                    state,
                    &self.user_id,
                    &self.space_id,
                    self.is_group,
                    draft,
                    &self.text,
                )
                .await?;
                let Some(reply) = reply else {
                    return Ok((Vec::new(), Outcome::default()));
                };
                let send = self.addressed_to_drafter(reply).await?;
                let outcome = Outcome {
                    reply_text: Some(send.text.clone()),
                    ..Outcome::default()
                };
                return Ok((vec![send], outcome));
            }
            Work::Answer | Work::Trip { .. } => {
                Turn::new(
                    state,
                    &self.user_id,
                    &self.space_id,
                    self.is_group,
                    self.circle_id.as_deref(),
                    self.sender_name.as_deref(),
                    self.sender_username.as_deref(),
                )
                .await?
            }
        };
        let answer = match &self.work {
            Work::Trip { mention, addressed } => {
                sends.extend(trips::on_trip(&mut turn, &self.text, mention).await?);
                sends.is_empty() && *addressed
            }
            _ => true,
        };
        if answer {
            let reply = turn::answer(&mut turn, &self.text).await;
            sends.push(SendRequest {
                space_id: self.space_id.clone(),
                text: reply.text,
                buttons: reply.buttons,
            });
        }
        let reply_text = joined(&sends);
        sends.append(&mut turn.sends);
        let outcome = Outcome {
            reply_text,
            calls: std::mem::take(&mut turn.calls),
            model: turn.model.take(),
            error: turn.fallback_reason.take(),
        };
        Ok((sends, outcome))
    }
}

impl Job {
    /// A draft's reply in this chat, its drop-off as this chat may show it; in a group it starts
    /// with the drafter's name, so the others see whose order it is.
    async fn addressed_to_drafter(&self, reply: ordering::DraftReply) -> ApiResult<SendRequest> {
        let mut send = reply.into_send(&self.space_id);
        if self.is_group {
            let name: Option<String> =
                sqlx::query_scalar("SELECT display_name FROM ferry.users WHERE id = $1")
                    .bind(&self.user_id)
                    .fetch_optional(&self.state.pool)
                    .await?;
            if let Some(name) = name.filter(|n| !n.trim().is_empty()) {
                send.text = format!("{}: {}", name.trim(), send.text);
            }
        }
        Ok(send)
    }
}

fn joined(sends: &[SendRequest]) -> Option<String> {
    (!sends.is_empty()).then(|| {
        sends
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// A press of an invite DM's "I'm in" or "Can't make it", answered through invites' respond with
/// the app's rules (which relays the answer to the inviter); the reply goes to the chat the button
/// was in.
async fn invite_button(
    state: &AppState,
    space_id: &str,
    user_id: &str,
    data: &str,
) -> ApiResult<SendRequest> {
    let line = |text: &str, buttons: Vec<Button>| SendRequest {
        space_id: space_id.to_owned(),
        text: text.to_owned(),
        buttons,
    };
    let accept = invites::parse_button(data).is_some_and(|(_, accept)| accept);
    match invites::respond_from_button(state, user_id, data).await {
        Ok(record) => {
            let plan = record.plan();
            let (say, button) = tools::invite_answer(
                &record,
                user_id,
                accept,
                &record.from.display_name,
                plan.as_deref(),
            );
            Ok(line(&say, button.into_iter().collect()))
        }
        Err(err) => Ok(line(&invite_refusal(&err), Vec::new())),
    }
}

/// The line for an invite answer invites refused.
fn invite_refusal(err: &ApiError) -> String {
    match err.code {
        ErrorCode::Conflict => "This invite has expired.".to_owned(),
        ErrorCode::Forbidden => err.message.clone(),
        ErrorCode::NotFound | ErrorCode::ValidationFailed => {
            "This invite isn't one you can answer.".to_owned()
        }
        _ => SNAG.to_owned(),
    }
}

/// Sends `sends` in order on a task.
pub fn spawn_deliver(state: &AppState, sends: Vec<SendRequest>) {
    if sends.is_empty() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move { deliver(&state, &sends).await });
}

/// Sends `sends` through the sidecar, one after another so they arrive in order. A chat the
/// sidecar reports unreachable (403) is marked so ([`spaces::mark_unreachable`]); other failures
/// are logged by the sidecar client and dropped, never retried (a send may have landed).
pub async fn deliver(state: &AppState, sends: &[SendRequest]) {
    for send in sends {
        if let Err(err) = sidecar::send(&state.http, &state.config, send).await
            && err.is_forbidden()
            && let Err(err) = spaces::mark_unreachable(&state.pool, &send.space_id).await
        {
            tracing::warn!(space = %mask_space_id(&send.space_id), code = ?err.code, "marking a chat unreachable failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_panicking_turn_is_caught_and_a_slow_one_stopped() {
        assert_eq!(run_caught(async { 7 }, Duration::from_secs(1)).await, Ok(7));
        let panicked = run_caught(
            async {
                let empty: Vec<u8> = Vec::new();
                empty[usize::from(empty.is_empty())]
            },
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(panicked, Err("panic"));
        let slow = run_caught(
            tokio::time::sleep(Duration::from_secs(5)),
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(slow, Err("work_timeout"));
    }
}
