//! Meet-again with a place and a time (Sal, 2026-10-11: "a ping with a place and a time, not a
//! way back into an old group"): what start_meet_again and the fallback share.
//!
//! - [`resolve_place`]: a restaurant from the catalog, else a known place (`GET /v1/places`), else
//!   the words as a label without a location. Several matches ask "Which one: ...?".
//! - [`resolve_time`]: the words' time on Chapel Hill's clock, by code
//!   ([`patterns::meetup_minutes`] and Postgres's `America/New_York`), never a timestamp from
//!   Gemini. Gemini's `when` is words ("at 7", "8 tomorrow"), read by [`read_when`].
//! - Who: in a group, the members of the group's circle; in a DM, the people from the user's most
//!   recent circle ([`circle_for`]).
//! - Without Gemini, the fallback reads "anyone want to go to Cosmic at 7?" with
//!   [`patterns::meet_mention`] and asks once before sending ([`ask`]): "Ask Jordan, Leo and Maya
//!   to go to Cosmic Cantina at 7:00 PM?" with Send and Cancel (`meet:<draft id>:send|cancel`).
//!   The draft waits in `ferry.meet_drafts`; only the asker's Send creates the invite
//!   ([`on_button`]), and a press claims the draft first, so two presses never send twice.

use sqlx::PgPool;

use super::patterns::{self, MeetMention, PlaceMatch, When, match_place};
use super::tools::and_list;
use super::turn::{Reply, Turn};
use crate::circles;
use crate::error::{ApiError, ApiResult, ErrorCode};
use crate::invites::{self, Created, InvitePlace, NewInvite};
use crate::messaging::sidecar::{Button, SendRequest};
use crate::orders::pricing::MAX_LABEL_CHARS;
use crate::state::AppState;

/// The first part of the question's button data: `meet:<draft id>:send|cancel`.
pub const BUTTON_PREFIX: &str = "meet";
/// How long the question can be answered with Send, in minutes.
pub const ASK_MINUTES: i32 = 30;

pub const NOT_YOURS: &str = "Only the person asking can answer that.";
pub const CANCELLED: &str = "OK, I won't send it.";
pub const GONE: &str = "That question is gone. Ask me again.";
pub const EXPIRED: &str = "That question expired. Ask me again.";
pub const ALREADY_SENT: &str = "That's sent already.";
pub const ALREADY_CANCELLED: &str = "That was cancelled. Ask me again.";
pub const PASSED: &str = "That time has passed. Ask me again with a new time.";

/// The button data for draft `id`.
pub fn button_data(id: i64, send: bool) -> String {
    format!(
        "{BUTTON_PREFIX}:{id}:{}",
        if send { "send" } else { "cancel" }
    )
}

/// Reads `meet:<id>:send|cancel` back.
pub fn parse_button(data: &str) -> Option<(i64, bool)> {
    let mut parts = data.split(':');
    let (prefix, id, action) = (parts.next()?, parts.next()?, parts.next()?);
    if prefix != BUTTON_PREFIX || parts.next().is_some() {
        return None;
    }
    let id = id.parse().ok().filter(|id| *id > 0)?;
    match action {
        "send" => Some((id, true)),
        "cancel" => Some((id, false)),
        _ => None,
    }
}

/// Whether `words` hold a date or a timestamp rather than a time in words: a run of four digits
/// ("2026"), or digits joined by `-`, `/` or `T` ("10-11", "11T19").
fn looks_like_a_date(words: &str) -> bool {
    let chars: Vec<char> = words.chars().collect();
    let run = chars
        .split(|c| !c.is_ascii_digit())
        .any(|digits| digits.len() >= 4);
    let joined = chars.windows(3).any(|w| {
        w[0].is_ascii_digit() && matches!(w[1], '-' | '/' | 'T' | 't') && w[2].is_ascii_digit()
    });
    run || joined
}

/// Gemini's `when` (the user's words: "at 7", "7pm", "8 tomorrow", "in 30 min") as a time and
/// whether it says tomorrow. A bare "7" reads as "at 7". Words with no digit and no time
/// ("tonight", "later") mean no time. Err with what to tell the user for a date or a timestamp
/// (never trusted) and for digits that are not a time.
pub fn read_when(words: &str) -> Result<(Option<When>, bool), String> {
    let words = words.trim();
    if looks_like_a_date(words) {
        return Err("Tell me the time the way you'd say it, like \"at 7pm\".".to_owned());
    }
    let tomorrow = patterns::says_tomorrow(words);
    let when = patterns::parse_when(words).or_else(|| patterns::parse_when(&format!("at {words}")));
    if when.is_none() && words.chars().any(|c| c.is_ascii_digit()) {
        return Err(format!(
            "I can't tell the time from \"{words}\". Say it like \"at 7pm\"."
        ));
    }
    Ok((when, tomorrow))
}

/// The words of a place as an invite's place: a restaurant's or known place's id, a restaurant
/// matched by name as a trip's place is ("cosmic", "sup dogs"), a known place matched by its
/// label ("davis library"), or else the words themselves as a label with no location. 404
/// `not_found` with "Which one: ...?" when the words fit several; 400 for empty or overlong words.
pub async fn resolve_place(pool: &PgPool, words: &str) -> ApiResult<InvitePlace> {
    let words = words
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| matches!(c, '"' | '“' | '”' | '*' | '_' | '.' | ',' | '!' | '?'))
        .to_owned();
    if words.is_empty() {
        return Err(ApiError::validation("Where to?"));
    }
    if words.starts_with("rst_") || words.starts_with("place_") {
        return invites::catalog_place(pool, &words)
            .await?
            .ok_or_else(|| ApiError::not_found("I don't know that place."));
    }
    let restaurants: Vec<(String, String)> =
        sqlx::query_as("SELECT id, name FROM ferry.restaurants ORDER BY name, id")
            .fetch_all(pool)
            .await?;
    let places: Vec<(String, String)> =
        sqlx::query_as("SELECT id, label FROM ferry.places ORDER BY label, id")
            .fetch_all(pool)
            .await?;
    for list in [&restaurants, &places] {
        match match_place(&words, list) {
            PlaceMatch::One(id) => {
                return invites::catalog_place(pool, &id)
                    .await?
                    .ok_or_else(|| ApiError::not_found("I don't know that place."));
            }
            PlaceMatch::Several(ids) => {
                let names: Vec<String> = list
                    .iter()
                    .filter(|(id, _)| ids.contains(id))
                    .map(|(_, name)| name.clone())
                    .collect();
                return Err(ApiError::not_found(format!(
                    "Which one: {}?",
                    and_list(&names).replace(" and ", " or ")
                )));
            }
            PlaceMatch::None => {}
        }
    }
    if words.chars().count() > MAX_LABEL_CHARS {
        return Err(ApiError::validation("That place's name is too long."));
    }
    Ok(InvitePlace {
        id: None,
        label: words,
        location: None,
    })
}

/// When a meetup is ([`patterns::meetup_minutes`] on Chapel Hill's clock now), as RFC 3339 UTC
/// for `proposed_time`. None for no time, and for "now".
pub async fn resolve_time(
    pool: &PgPool,
    when: Option<When>,
    tomorrow: bool,
) -> ApiResult<Option<String>> {
    let Some(when) = when else {
        return Ok(None);
    };
    let now_minutes: i32 = sqlx::query_scalar(
        "SELECT (extract(hour FROM t) * 60 + extract(minute FROM t))::int
           FROM (SELECT now() AT TIME ZONE 'America/New_York' AS t) local",
    )
    .fetch_one(pool)
    .await?;
    let now_minutes = u32::try_from(now_minutes).unwrap_or(0);
    let Some(minutes) = patterns::meetup_minutes(when, now_minutes, tomorrow) else {
        return Ok(None);
    };
    let at: String = sqlx::query_scalar(
        "SELECT to_char(((date_trunc('day', now() AT TIME ZONE 'America/New_York')
                          + make_interval(mins => $1)) AT TIME ZONE 'America/New_York')
                        AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
    )
    .bind(i32::try_from(minutes).unwrap_or(i32::MAX))
    .fetch_one(pool)
    .await?;
    Ok(Some(at))
}

/// The circle a meet-again goes to: the group's in a group, else the user's most recent circle
/// with someone else in it. 404 `not_found` when there is none.
pub async fn circle_for(turn: &Turn) -> ApiResult<String> {
    if let Some(id) = &turn.circle_id {
        return Ok(id.clone());
    }
    circles::most_recent_circle(&turn.state.pool, &turn.user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("You're not in a circle with anyone yet."))
}

/// The line after an invite goes out: "Pinged Jordan, Leo and Maya: Cosmic Cantina at 7:00 PM.
/// I'll tell you who's in.", then who gets it in the app only. `plan` is
/// [`invites::InviteRecord::plan`].
pub fn sent_line(invited: &[String], app_only: &[String], plan: Option<&str>) -> String {
    let mut line = match plan {
        Some(plan) => format!(
            "Pinged {}: {plan}. I'll tell you who's in.",
            and_list(invited)
        ),
        None => format!(
            "Pinged {} to meet up. I'll tell you who's in.",
            and_list(invited)
        ),
    };
    match app_only {
        [] => {}
        [one] => line.push_str(&format!(" {one} gets it in the app only.")),
        several => line.push_str(&format!(" {} get it in the app only.", and_list(several))),
    }
    line
}

/// The question the fallback asks: "Ask Jordan, Leo and Maya to go to Cosmic Cantina at 7:00 PM?".
pub fn question(invitees: &[String], place: Option<&str>, when: Option<&str>) -> String {
    let what = match (place, when) {
        (Some(place), Some(when)) => format!("go to {place} at {when}"),
        (Some(place), None) => format!("go to {place}"),
        (None, Some(when)) => format!("meet up at {when}"),
        (None, None) => "meet up".to_owned(),
    };
    format!("Ask {} to {what}?", and_list(invitees))
}

/// A display name, or "A friend" for someone who has not set one.
fn shown(name: &str) -> String {
    match name.trim() {
        "" => "A friend".to_owned(),
        name => name.to_owned(),
    }
}

/// What the bot says for a refusal from invites or from the place and time readers.
fn refusal(err: &ApiError) -> String {
    match err.code {
        ErrorCode::NotFound | ErrorCode::Conflict | ErrorCode::Forbidden => err.message.clone(),
        ErrorCode::ValidationFailed if err.message.contains("proposed_time") => PASSED.to_owned(),
        ErrorCode::ValidationFailed => err.message.clone(),
        _ => "I couldn't set that up just now. Try again in a moment.".to_owned(),
    }
}

/// The fallback's answer to a meet-again in words (Gemini down): reads the place and the time,
/// records a draft, and asks once, "Ask Jordan, Leo and Maya to go to Cosmic Cantina at 7:00 PM?",
/// with Send and Cancel. Nothing is sent until the asker presses Send.
pub async fn ask(turn: &mut Turn, mention: &MeetMention) -> Reply {
    match ask_with_buttons(turn, mention).await {
        Ok((text, buttons)) => {
            turn.add_buttons(&buttons);
            turn.reply(&text)
        }
        Err(err) => {
            tracing::info!(code = ?err.code, "meet-again question refused");
            turn.reply(&refusal(&err))
        }
    }
}

async fn ask_with_buttons(turn: &Turn, mention: &MeetMention) -> ApiResult<(String, Vec<Button>)> {
    let pool = &turn.state.pool;
    let circle_id = circle_for(turn).await?;
    let place = match &mention.place {
        Some(words) => Some(resolve_place(pool, words).await?),
        None => None,
    };
    let proposed_time = resolve_time(pool, mention.when, mention.tomorrow).await?;
    let invitees: Vec<String> = sqlx::query_scalar(
        "SELECT u.display_name FROM ferry.circle_members m JOIN ferry.users u ON u.id = m.user_id
          WHERE m.circle_id = $1 AND m.user_id <> $2
          ORDER BY m.joined_at, m.user_id",
    )
    .bind(&circle_id)
    .bind(&turn.user_id)
    .fetch_all(pool)
    .await?;
    if invitees.is_empty() {
        return Err(ApiError::conflict(
            "Nobody else is in this circle yet, so there is no one to invite.",
        ));
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO ferry.meet_drafts
                (user_id, space_id, circle_id, place_id, place_label, place_lat, place_lng,
                 proposed_time)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8::timestamptz)
         RETURNING id",
    )
    .bind(&turn.user_id)
    .bind(&turn.space_id)
    .bind(&circle_id)
    .bind(place.as_ref().and_then(|p| p.id.as_deref()))
    .bind(place.as_ref().map(|p| p.label.as_str()))
    .bind(place.as_ref().and_then(|p| p.location).map(|at| at.lat))
    .bind(place.as_ref().and_then(|p| p.location).map(|at| at.lng))
    .bind(proposed_time.as_deref())
    .fetch_one(pool)
    .await?;
    let when = match &proposed_time {
        Some(at) => Some(invites::clock_words(pool, at).await?),
        None => None,
    };
    let invitees: Vec<String> = invitees.iter().map(|n| shown(n)).collect();
    tracing::info!(
        draft = id,
        place = place.is_some(),
        timed = when.is_some(),
        "meet-again asked"
    );
    Ok((
        question(
            &invitees,
            place.as_ref().map(|p| p.label.as_str()),
            when.as_deref(),
        ),
        vec![
            Button::callback("Send", button_data(id, true)),
            Button::callback("Cancel", button_data(id, false)),
        ],
    ))
}

#[derive(sqlx::FromRow)]
struct DraftRow {
    user_id: String,
    circle_id: String,
    place_id: Option<String>,
    place_label: Option<String>,
    place_lat: Option<f64>,
    place_lng: Option<f64>,
    proposed_time: Option<String>,
    status: String,
    fresh: bool,
}

/// A press of the question's Send or Cancel (`meet:<id>:send|cancel`) by `user_id` in `space_id`.
/// Only the asker's press counts. Send claims the draft (a conditional update from `asked`), then
/// creates the invite exactly as the agent's tool does; a refusal (a time that has passed) puts
/// the draft back. Returns the reply for the chat.
pub async fn on_button(
    state: &AppState,
    space_id: &str,
    user_id: &str,
    data: &str,
) -> ApiResult<SendRequest> {
    let line = |text: String| SendRequest {
        space_id: space_id.to_owned(),
        text,
        buttons: Vec::new(),
    };
    let (id, send) = parse_button(data)
        .ok_or_else(|| ApiError::validation("That is not a meet-again button."))?;
    let draft: Option<DraftRow> = sqlx::query_as(
        "SELECT user_id, circle_id, place_id, place_label, place_lat, place_lng,
                to_char(proposed_time AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')
                    AS proposed_time,
                status, created_at > now() - make_interval(mins => $2) AS fresh
           FROM ferry.meet_drafts WHERE id = $1",
    )
    .bind(id)
    .bind(ASK_MINUTES)
    .fetch_optional(&state.pool)
    .await?;
    let Some(draft) = draft else {
        return Ok(line(GONE.to_owned()));
    };
    if draft.user_id != user_id {
        return Ok(line(NOT_YOURS.to_owned()));
    }
    let decided = |status: &str| match status {
        "sent" => ALREADY_SENT,
        _ => ALREADY_CANCELLED,
    };
    if !send {
        let cancelled: Option<i64> = sqlx::query_scalar(
            "UPDATE ferry.meet_drafts SET status = 'cancelled', decided_at = now()
              WHERE id = $1 AND status = 'asked' RETURNING id",
        )
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
        return Ok(line(match cancelled {
            Some(_) => CANCELLED.to_owned(),
            None => decided(&draft.status).to_owned(),
        }));
    }
    if draft.status != "asked" {
        return Ok(line(decided(&draft.status).to_owned()));
    }
    if !draft.fresh {
        return Ok(line(EXPIRED.to_owned()));
    }
    let claimed: Option<i64> = sqlx::query_scalar(
        "UPDATE ferry.meet_drafts SET status = 'sent', decided_at = now()
          WHERE id = $1 AND status = 'asked' RETURNING id",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    if claimed.is_none() {
        return Ok(line(ALREADY_SENT.to_owned()));
    }
    let place = draft.place_label.map(|label| InvitePlace {
        id: draft.place_id,
        label,
        location: draft
            .place_lat
            .zip(draft.place_lng)
            .map(|(lat, lng)| crate::catalog::types::LatLng { lat, lng }),
    });
    let new = NewInvite {
        message: None,
        proposed_time: draft.proposed_time,
        place,
    };
    match invites::create_invite(state, &draft.circle_id, user_id, new).await {
        Ok(Created { record, app_only }) => {
            sqlx::query("UPDATE ferry.meet_drafts SET invite_id = $2 WHERE id = $1")
                .bind(id)
                .bind(&record.id)
                .execute(&state.pool)
                .await?;
            let invited: Vec<String> = record
                .responses
                .iter()
                .map(|r| shown(&r.user.display_name))
                .collect();
            let app_only: Vec<String> = record
                .responses
                .iter()
                .filter(|r| app_only.contains(&r.user.id))
                .map(|r| shown(&r.user.display_name))
                .collect();
            tracing::info!(
                draft = id,
                invite_id = record.id,
                "meet-again sent after the question"
            );
            Ok(line(sent_line(
                &invited,
                &app_only,
                record.plan().as_deref(),
            )))
        }
        Err(err) => {
            sqlx::query(
                "UPDATE ferry.meet_drafts SET status = 'asked', decided_at = NULL
                  WHERE id = $1 AND status = 'sent' AND invite_id IS NULL",
            )
            .bind(id)
            .execute(&state.pool)
            .await?;
            tracing::info!(draft = id, code = ?err.code, "meet-again refused after the question");
            Ok(line(refusal(&err)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_round_trip() {
        for send in [true, false] {
            let data = button_data(9_007_199_254_740_991, send);
            assert!(data.len() <= 64, "{data}");
            assert_eq!(parse_button(&data), Some((9_007_199_254_740_991, send)));
        }
        assert_eq!(button_data(7, true), "meet:7:send");
        assert_eq!(button_data(7, false), "meet:7:cancel");
        for bad in [
            "",
            "meet",
            "meet:7",
            "meet:7:yes",
            "meet:0:send",
            "meet:-1:send",
            "meet:x:send",
            "meet:7:send:x",
            "trip:7:send",
            "invite:inv_x:accept",
        ] {
            assert_eq!(parse_button(bad), None, "{bad}");
        }
    }

    #[test]
    fn gemini_says_when_in_words_and_code_reads_it() {
        let at = |hour, meridiem| When::At {
            hour,
            minute: 0,
            meridiem,
        };
        assert_eq!(read_when("at 7"), Ok((Some(at(7, None)), false)));
        assert_eq!(read_when("7"), Ok((Some(at(7, None)), false)));
        assert_eq!(
            read_when("7pm"),
            Ok((Some(at(19, Some(patterns::Meridiem::Pm))), false))
        );
        assert_eq!(read_when("8 tomorrow"), Ok((Some(at(8, None)), true)));
        assert_eq!(read_when("at 8 tmrw"), Ok((Some(at(8, None)), true)));
        assert_eq!(
            read_when("in 30 min"),
            Ok((Some(When::In { minutes: 30 }), false))
        );
        assert_eq!(read_when("tonight"), Ok((None, false)));
        assert_eq!(read_when("tomorrow"), Ok((None, true)));
        for date in [
            "2026-10-11T19:00:00-04:00",
            "2026-10-11T23:00:00Z",
            "10/12 at 7",
            "10-12 7pm",
            "19:00 2026",
        ] {
            assert!(read_when(date).is_err(), "{date}");
        }
        assert!(read_when("at 75").is_err());
    }

    #[test]
    fn lines() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            sent_line(
                &s(&["Jordan", "Leo", "Maya"]),
                &s(&["Leo"]),
                Some("Cosmic Cantina at 7:00 PM")
            ),
            "Pinged Jordan, Leo and Maya: Cosmic Cantina at 7:00 PM. I'll tell you who's in. Leo \
             gets it in the app only."
        );
        assert_eq!(
            sent_line(&s(&["Jordan", "Maya"]), &s(&["Jordan", "Maya"]), None),
            "Pinged Jordan and Maya to meet up. I'll tell you who's in. Jordan and Maya get it in \
             the app only."
        );
        let people = s(&["Jordan", "Leo", "Maya"]);
        assert_eq!(
            question(&people, Some("Cosmic Cantina"), Some("7:00 PM")),
            "Ask Jordan, Leo and Maya to go to Cosmic Cantina at 7:00 PM?"
        );
        assert_eq!(
            question(&people, Some("Sup Dogs"), None),
            "Ask Jordan, Leo and Maya to go to Sup Dogs?"
        );
        assert_eq!(
            question(&people, None, Some("8:00 PM tomorrow")),
            "Ask Jordan, Leo and Maya to meet up at 8:00 PM tomorrow?"
        );
        assert_eq!(
            question(&people, None, None),
            "Ask Jordan, Leo and Maya to meet up?"
        );
    }
}
