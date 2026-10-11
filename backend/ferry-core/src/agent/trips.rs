//! Trip detection (spec section 3): "heading to Cosmic Cantina at 6, anyone want anything?".
//!
//! 1. [`super::patterns::prefilter`] catches the phrasing (cheap, no network), and only then
//! 2. Gemini classifies the scrubbed message against the restaurant list (a JSON answer:
//!    `is_trip` and `restaurant_id`). Without Gemini, the place is matched against the restaurant
//!    names by the patterns alone, and only an unambiguous match counts.
//! 3. When the trip is sure (Gemini says it is a trip and the place names that restaurant
//!    plainly), the bot opens the run at once (`runs::open_run`, created via message) and replies
//!    "Ferry to Cosmic Cantina is open until 6:10. Reply with your order or tap the button.", with
//!    `{web_url}/r/{run_id}` as a link button. When the announcer already has an open run at that
//!    restaurant in that circle (the demo's seeded run_demo), that run is the one announced.
//! 4. Otherwise it asks once: "Open a Ferry to Cosmic Cantina? Orders close 6:10.", with Yes and
//!    No buttons, and Yes opens it. A trip the same person already announced in the same chat for
//!    the same restaurant within [`ASK_ONCE_HOURS`] gets nothing more while it is live: asked or
//!    confirmed with its window not passed, or opened with its run still open. A trip whose run
//!    closed, was cancelled or was deleted (the demo reset) is announced again.
//!
//! Every trip is recorded in `ferry.trip_intents`, with the run once one opens; the run's later
//! state changes are announced in the same chat (`super::announce`).
//!
//! Times are Chapel Hill's: "at 6" is the next 6:00 (am or pm) not more than 30 minutes past,
//! a trip with no time leaves in [`DEFAULT_LEAVE_MINUTES`], and orders close
//! [`WINDOW_AFTER_LEAVING_MINUTES`] after leaving, as the demo's "at 6 ... open until 6:10". A
//! time already past leaves now.

use serde_json::{Value, json};

use super::gemini::{self, Gemini};
use super::log::CallLog;
use super::patterns::{self, PlaceMatch, TripMention, When};
use super::tools::link_button;
use super::turn::{Turn, elapsed_ms};
use crate::circles;
use crate::error::{ApiError, ApiResult, ErrorCode};
use crate::messaging::sidecar::{Button, SendRequest};
use crate::runs::{self, CreateRunRequest, CreatedVia};
use crate::state::AppState;

/// A trip with no time leaves this many minutes from now (the demo seed's run_demo too).
pub const DEFAULT_LEAVE_MINUTES: u32 = 15;
/// Orders close this long after the runner leaves.
pub const WINDOW_AFTER_LEAVING_MINUTES: u32 = 10;
/// How long one question covers a trip.
pub const ASK_ONCE_HOURS: i32 = 2;
/// The first part of the question's button data: `trip:<intent id>:yes|no`.
pub const BUTTON_PREFIX: &str = "trip";

/// The button data for intent `id`.
pub fn button_data(id: i64, yes: bool) -> String {
    format!("{BUTTON_PREFIX}:{id}:{}", if yes { "yes" } else { "no" })
}

/// Reads `trip:<id>:yes|no` back.
pub fn parse_button(data: &str) -> Option<(i64, bool)> {
    let mut parts = data.split(':');
    let (prefix, id, answer) = (parts.next()?, parts.next()?, parts.next()?);
    if prefix != BUTTON_PREFIX || parts.next().is_some() {
        return None;
    }
    let id = id.parse().ok().filter(|id| *id > 0)?;
    match answer {
        "yes" => Some((id, true)),
        "no" => Some((id, false)),
        _ => None,
    }
}

/// The question the bot asks when it is not sure.
pub fn question(restaurant: &str, closes: &str) -> String {
    format!("Open a Ferry to {restaurant}? Orders close {closes}.")
}

/// What the bot says when a run opens (docs/DEMO.md, Channel: Telegram, step 1).
pub fn opened(restaurant: &str, closes: &str) -> String {
    format!(
        "Ferry to {restaurant} is open until {closes}. Reply with your order or tap the button."
    )
}

/// The restaurant as a trip's lines name it: in a public community (`public`, its name; a DM
/// trip from someone with no other circle lands in UNC Chapel Hill), it says so, so nobody posts
/// to everyone without being told.
pub fn place_for(restaurant: &str, public: Option<&str>) -> String {
    match public {
        Some(name) => format!("{restaurant} for everyone in {name}"),
        None => restaurant.to_owned(),
    }
}

/// The name of `circle_id` when it is a public community, from `ferry.circles`; None otherwise.
async fn public_name(pool: &sqlx::PgPool, circle_id: Option<&str>) -> ApiResult<Option<String>> {
    let Some(id) = circle_id else {
        return Ok(None);
    };
    Ok(
        sqlx::query_scalar("SELECT name FROM ferry.circles WHERE id = $1 AND is_public")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

pub const DECLINED: &str = "OK, no Ferry this time.";
pub const NOT_YOURS: &str = "Only the person heading out can answer that.";
pub const NO_CIRCLE: &str = "Add me to your group first, so I know who the Ferry is for.";

/// A trip `turn`'s user announced in `text` (`mention` from the prefilter). Returns what to send:
/// the opened run, the question, or nothing when it is not a trip to a known restaurant or was
/// handled already.
pub async fn on_trip(
    turn: &mut Turn,
    text: &str,
    mention: &TripMention,
) -> ApiResult<Vec<SendRequest>> {
    let restaurants: Vec<(String, String, Vec<String>)> =
        sqlx::query_as("SELECT id, name, cuisine FROM ferry.restaurants ORDER BY name, id")
            .fetch_all(&turn.state.pool)
            .await?;
    let pairs: Vec<(String, String)> = restaurants
        .iter()
        .map(|(id, name, _)| (id.clone(), name.clone()))
        .collect();
    let by_patterns = patterns::match_place(&mention.place, &pairs);

    let classified = match Gemini::for_app(&turn.state) {
        Some(gemini) => {
            turn.model = Some(gemini.model().to_owned());
            classify(turn, &gemini, text, &restaurants).await
        }
        None => Err("gemini_unconfigured"),
    };
    let (restaurant_id, detected_by) = match classified {
        Ok(Some(id)) => (id, "gemini"),
        Ok(None) => {
            tracing::debug!("not a trip, says Gemini");
            return Ok(Vec::new());
        }
        Err(reason) => {
            turn.fallback_reason = Some(reason.to_owned());
            match &by_patterns {
                PlaceMatch::One(id) => (id.clone(), "patterns"),
                _ => return Ok(Vec::new()),
            }
        }
    };
    // Sure: Gemini's reading and the plain words agree.
    let sure = detected_by == "gemini" && by_patterns == PlaceMatch::One(restaurant_id.clone());
    let restaurant = pairs
        .iter()
        .find(|(id, _)| *id == restaurant_id)
        .map(|(_, name)| name.clone())
        .unwrap_or_default();

    // Only a live intent counts: a stale one (its window passed, its run gone or done) never
    // silences a new trip.
    let handled: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM ferry.trip_intents t
                         WHERE t.space_id = $1 AND t.user_id = $2 AND t.restaurant_id = $3
                           AND t.created_at > now() - make_interval(hours => $4)
                           AND ((t.status IN ('asked', 'confirmed') AND t.window_closes_at > now())
                                OR (t.status = 'opened' AND EXISTS (
                                      SELECT 1 FROM ferry.runs r
                                       WHERE r.id = t.run_id AND r.status = 'open'
                                         AND r.window_closes_at > now()))))",
    )
    .bind(&turn.space_id)
    .bind(&turn.user_id)
    .bind(&restaurant_id)
    .bind(ASK_ONCE_HOURS)
    .fetch_one(&turn.state.pool)
    .await?;
    if handled {
        tracing::debug!("trip already handled");
        return Ok(Vec::new());
    }

    let (id, closes, circle_id) =
        record_intent(turn, &restaurant_id, mention.when, detected_by).await?;
    tracing::info!(
        intent = id,
        restaurant_id,
        detected_by,
        sure,
        "trip detected"
    );

    if sure {
        let reply = open_for_intent(&turn.state, id).await?;
        return Ok(vec![reply]);
    }
    let public = public_name(&turn.state.pool, circle_id.as_deref()).await?;
    Ok(vec![SendRequest {
        space_id: turn.space_id.clone(),
        text: question(
            &place_for(&restaurant, public.as_deref()),
            &patterns::clock(closes),
        ),
        buttons: vec![
            Button::callback("Yes", button_data(id, true)),
            Button::callback("No", button_data(id, false)),
        ],
    }])
}

/// Records a trip intent for `turn`'s user and chat: the circle (the group's; in a DM the user's
/// most recent circle with someone else in it, else UNC Chapel Hill when they are in it), when they leave
/// and when orders close. Returns its id, the closing time in minutes after Chapel Hill's
/// midnight and the circle.
async fn record_intent(
    turn: &Turn,
    restaurant_id: &str,
    when: Option<When>,
    detected_by: &str,
) -> ApiResult<(i64, u32, Option<String>)> {
    let circle_id = match &turn.circle_id {
        Some(id) => Some(id.clone()),
        None => match circles::most_recent_circle(&turn.state.pool, &turn.user_id).await? {
            Some(id) => Some(id),
            None => in_campus(&turn.state.pool, &turn.user_id)
                .await?
                .then(|| circles::CAMPUS_CIRCLE_ID.to_owned()),
        },
    };
    let now_minutes: i32 = sqlx::query_scalar(
        "SELECT (extract(hour FROM t) * 60 + extract(minute FROM t))::int
           FROM (SELECT now() AT TIME ZONE 'America/New_York' AS t) local",
    )
    .fetch_one(&turn.state.pool)
    .await?;
    let now_minutes = u32::try_from(now_minutes).unwrap_or(0);
    let when = when.unwrap_or(When::In {
        minutes: DEFAULT_LEAVE_MINUTES,
    });
    let leaving = patterns::resolve_minutes(when, now_minutes).max(now_minutes);
    let closes = leaving + WINDOW_AFTER_LEAVING_MINUTES;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO ferry.trip_intents
                (space_id, user_id, circle_id, restaurant_id, leaving_at, window_closes_at, detected_by)
         SELECT $1, $2, $3, $4, l.at, l.at + make_interval(mins => $6), $7
           FROM (SELECT (date_trunc('day', now() AT TIME ZONE 'America/New_York')
                         + make_interval(mins => $5)) AT TIME ZONE 'America/New_York' AS at) l
         RETURNING id",
    )
    .bind(&turn.space_id)
    .bind(&turn.user_id)
    .bind(circle_id.as_deref())
    .bind(restaurant_id)
    .bind(i32::try_from(leaving).unwrap_or(i32::MAX))
    .bind(i32::try_from(WINDOW_AFTER_LEAVING_MINUTES).unwrap_or(10))
    .bind(detected_by)
    .fetch_one(&turn.state.pool)
    .await?;
    Ok((id, closes, circle_id))
}

/// Whether `user_id` is in UNC Chapel Hill (every user is, unless a test removed them).
async fn in_campus(pool: &sqlx::PgPool, user_id: &str) -> ApiResult<bool> {
    let member = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM ferry.circle_members WHERE circle_id = $1 AND user_id = $2)",
    )
    .bind(circles::CAMPUS_CIRCLE_ID)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(member)
}

/// Opens a run now for `turn`'s user at `restaurant_id` (the open_run tool): records the intent
/// and opens it, as a sure trip does. Returns the line that announces it.
pub async fn open_now(
    turn: &Turn,
    restaurant_id: &str,
    when: Option<When>,
) -> ApiResult<SendRequest> {
    let (id, _, _) = record_intent(turn, restaurant_id, when, "gemini").await?;
    open_for_intent(&turn.state, id).await
}

#[derive(sqlx::FromRow)]
struct IntentRun {
    space_id: String,
    user_id: String,
    circle_id: Option<String>,
    restaurant_id: String,
    restaurant: String,
    /// `leaving_at`, as RFC 3339 UTC, moved to now when it has passed.
    leaving_at: String,
}

/// Opens the run for intent `id` (or reuses the announcer's open run at that restaurant in that
/// circle), marks the intent opened, and returns the line that announces it in the intent's chat.
/// A refusal from runs (not a member, a window already closed) becomes the reply.
async fn open_for_intent(state: &AppState, id: i64) -> ApiResult<SendRequest> {
    let intent: IntentRun = sqlx::query_as(
        "SELECT t.space_id, t.user_id, t.circle_id, t.restaurant_id, r.name AS restaurant,
                to_char(GREATEST(t.leaving_at, now()) AT TIME ZONE 'UTC',
                        'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS leaving_at
           FROM ferry.trip_intents t JOIN ferry.restaurants r ON r.id = t.restaurant_id
          WHERE t.id = $1",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;
    let line = |text: String, buttons: Vec<Button>| SendRequest {
        space_id: intent.space_id.clone(),
        text,
        buttons,
    };
    let Some(circle_id) = intent.circle_id.clone() else {
        return Ok(line(NO_CIRCLE.to_owned(), Vec::new()));
    };
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM ferry.runs
          WHERE runner_id = $1 AND restaurant_id = $2 AND circle_id = $3
            AND status = 'open' AND window_closes_at > now()
          ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(&intent.user_id)
    .bind(&intent.restaurant_id)
    .bind(&circle_id)
    .fetch_optional(&state.pool)
    .await?;
    let run_id = match existing {
        Some(run_id) => run_id,
        None => {
            let request = CreateRunRequest {
                restaurant_id: intent.restaurant_id.clone(),
                circle_id,
                leaving_at: intent.leaving_at.clone(),
                window_minutes: i64::from(WINDOW_AFTER_LEAVING_MINUTES),
            };
            match runs::open_run(state, &intent.user_id, &request, CreatedVia::Message).await {
                Ok(run) => run.id,
                Err(err) => {
                    tracing::info!(intent = id, code = ?err.code, "the trip's run was refused");
                    return Ok(line(refusal(&err), Vec::new()));
                }
            }
        }
    };
    let closes: String = sqlx::query_scalar(
        "UPDATE ferry.trip_intents SET status = 'opened', run_id = $2, decided_at = now()
          WHERE id = $1
         RETURNING (SELECT to_char(window_closes_at AT TIME ZONE 'America/New_York', 'FMHH12:MI')
                      FROM ferry.runs WHERE id = $2)",
    )
    .bind(id)
    .bind(&run_id)
    .fetch_one(&state.pool)
    .await?;
    tracing::info!(intent = id, run_id, "trip's run open");
    let web = super::tools::web_url_of(&state.config);
    let url = format!("{web}/r/{run_id}");
    let text = opened(
        &place_for(
            &intent.restaurant,
            public_name(&state.pool, intent.circle_id.as_deref())
                .await?
                .as_deref(),
        ),
        &closes,
    );
    Ok(match link_button("Open in Ferry", &url) {
        Some(button) => line(text, vec![button]),
        None => line(format!("{text} {url}"), Vec::new()),
    })
}

/// A refusal from runs, in the bot's words.
fn refusal(err: &ApiError) -> String {
    match err.code {
        ErrorCode::NotFound => NO_CIRCLE.to_owned(),
        ErrorCode::ValidationFailed | ErrorCode::Conflict | ErrorCode::Forbidden => {
            format!("I couldn't open that Ferry: {}", err.message)
        }
        _ => "I couldn't open that Ferry just now. Try again in a moment.".to_owned(),
    }
}

/// The trip classifier's instruction, with the restaurant list (`- id: name (cuisines)` lines).
pub fn classifier_instruction(restaurants: &str) -> String {
    format!(
        "You read one message from a Telegram chat of UNC Chapel Hill students. People's names are \
replaced by placeholders in braces. Decide whether the sender is announcing that they are going to \
one of these restaurants soon, so friends could ask them to bring food back. Plans that are not \
the sender's own trip, questions about a place, and past trips are not trips.\n\
Restaurants:\n{restaurants}\n\
Answer with is_trip, and restaurant_id: the id of the restaurant they are going to, or \"none\"."
    )
}

/// Gemini's reading of `text`: Ok(Some(restaurant id)) for a trip to a listed restaurant,
/// Ok(None) for anything else, Err(reason) when Gemini could not answer.
async fn classify(
    turn: &mut Turn,
    gemini: &Gemini,
    text: &str,
    restaurants: &[(String, String, Vec<String>)],
) -> Result<Option<String>, &'static str> {
    let list: Vec<String> = restaurants
        .iter()
        .map(|(id, name, cuisine)| format!("- {id}: {name} ({})", cuisine.join(", ")))
        .collect();
    let system = classifier_instruction(&list.join("\n"));
    let mut ids: Vec<Value> = restaurants.iter().map(|(id, _, _)| json!(id)).collect();
    ids.push(json!("none"));
    let schema = json!({
        "type": "object",
        "properties": {
            "is_trip": {"type": "boolean"},
            "restaurant_id": {"type": "string", "enum": ids},
        },
        "required": ["is_trip", "restaurant_id"],
    });
    let contents = [gemini::user_text(&turn.redactor.scrub(text))];
    let body = gemini::build_json_request(&system, &contents, &schema, 1024);
    let sealed = turn.redactor.seal(body).map_err(|_| "privacy_guard")?;
    let started = std::time::Instant::now();
    let (result, attempts) = gemini.generate_counted(&sealed).await;
    turn.calls.push(CallLog {
        name: "gemini".into(),
        args: json!({"classify": "trip", "attempts": attempts}),
        ok: result.is_ok(),
        latency_ms: elapsed_ms(started),
        error: result.as_ref().err().map(|e| e.label().to_owned()),
    });
    let generated = result.map_err(|e| e.label())?;
    let answer = gemini::parse_json_answer(&generated).ok_or("bad_json")?;
    let is_trip = answer["is_trip"].as_bool().ok_or("bad_json")?;
    let id = answer["restaurant_id"].as_str().unwrap_or("none");
    let known = restaurants.iter().any(|(rid, _, _)| rid == id);
    Ok((is_trip && known).then(|| id.to_owned()))
}

#[derive(sqlx::FromRow)]
struct IntentRow {
    user_id: String,
    status: String,
}

/// A press of a trip question's Yes or No by `user_id`, in `space_id`. Only the announcer's
/// press counts, once; Yes opens the run. A press on an answered question says nothing.
pub async fn on_button(
    state: &AppState,
    space_id: &str,
    user_id: &str,
    data: &str,
) -> ApiResult<Vec<SendRequest>> {
    let Some((id, yes)) = parse_button(data) else {
        return Ok(Vec::new());
    };
    let line = |text: &str| SendRequest {
        space_id: space_id.to_owned(),
        text: text.to_owned(),
        buttons: Vec::new(),
    };
    let mut tx = state.pool.begin().await?;
    let row: Option<IntentRow> = sqlx::query_as(
        "SELECT user_id, status FROM ferry.trip_intents
          WHERE id = $1 AND space_id = $2
            FOR UPDATE",
    )
    .bind(id)
    .bind(space_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Ok(Vec::new());
    };
    if row.user_id != user_id {
        return Ok(vec![line(NOT_YOURS)]);
    }
    if row.status != "asked" {
        return Ok(Vec::new());
    }
    sqlx::query("UPDATE ferry.trip_intents SET status = $2, decided_at = now() WHERE id = $1")
        .bind(id)
        .bind(if yes { "confirmed" } else { "declined" })
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(intent = id, yes, "trip question answered");
    if !yes {
        return Ok(vec![line(DECLINED)]);
    }
    Ok(vec![open_for_intent(state, id).await?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_round_trip() {
        for yes in [true, false] {
            let data = button_data(42, yes);
            assert!(data.len() <= 64);
            assert_eq!(parse_button(&data), Some((42, yes)));
        }
        for bad in [
            "trip:0:yes",
            "trip:x:yes",
            "trip:1:maybe",
            "invite:1:yes",
            "trip:1:yes:x",
            "",
        ] {
            assert_eq!(parse_button(bad), None, "{bad}");
        }
    }

    #[test]
    fn lines() {
        assert_eq!(
            question("Cosmic Cantina", "6:10"),
            "Open a Ferry to Cosmic Cantina? Orders close 6:10."
        );
        assert_eq!(
            opened("Cosmic Cantina", "6:10"),
            "Ferry to Cosmic Cantina is open until 6:10. Reply with your order or tap the button."
        );
        let campus = place_for("Cosmic Cantina", Some("Test Town"));
        assert_eq!(
            question(&campus, "6:10"),
            "Open a Ferry to Cosmic Cantina for everyone in Test Town? Orders close 6:10."
        );
        assert_eq!(
            opened(&campus, "6:10"),
            "Ferry to Cosmic Cantina for everyone in Test Town is open until 6:10. Reply with your \
             order or tap the button."
        );
        assert_eq!(place_for("Cosmic Cantina", None), "Cosmic Cantina");
    }
}
