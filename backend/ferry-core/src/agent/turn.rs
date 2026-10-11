//! One agent turn: a user's message in, one reply out (spec section 3).
//!
//! With Gemini configured, the turn runs the tool loop: Gemini sees the scrubbed message, the
//! user's saved defaults and the tools' declarations, picks tools, and phrases the reply from
//! their results. At most [`MAX_TOOL_ROUNDS`] rounds of tool calls; after that one last call
//! without tools must answer. Every call is logged with its latency. Without Gemini, or when a
//! call fails after its retry, or when a payload would leak a name ([`super::privacy`]), the
//! deterministic [`super::fallback`] answers with the same tools.
//!
//! Facts come from code ([`final_text`]): when a tool with a `say` line ran, the user gets those
//! lines, and Gemini's own words only when they carry each line word for word and add no fact
//! (no digit, no money, no order's fate). With no tool run, words that state such a fact are
//! refused and the fallback answers. A reply Gemini cut short (`MAX_TOKENS`) is never sent.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::fallback;
use super::gemini::{self, Gemini, GeminiError, ToolMode};
use super::log::CallLog;
use super::ordering;
use super::privacy::Redactor;
use super::tools::{self, Tool};
use super::trips;
use crate::error::ApiResult;
use crate::messaging::sidecar::{Button, SendRequest};
use crate::profile::load_user;
use crate::state::AppState;

/// Rounds of tool calls per message.
pub const MAX_TOOL_ROUNDS: usize = 4;
/// A whole turn's budget for Gemini, counted from [`Turn::new`] and shared by the trip classifier
/// and the answer; once spent, the fallback answers. The classifier (at most 2 × 12 s + 300 ms)
/// runs first, so every Gemini call of a turn ends within about 25 s, well inside the job's 45 s
/// (`dispatch::WORK_TIMEOUT`).
pub const TURN_BUDGET: Duration = Duration::from_secs(25);
/// Less budget than this left after the classifier: the answer does not call Gemini.
const MIN_ANSWER_BUDGET: Duration = Duration::from_secs(2);
/// Gemini's output cap per call, in tokens. Replies are a sentence or two, but a thinking model
/// spends from the same budget before it answers, and a spent budget comes back empty.
const MAX_OUTPUT_TOKENS: u32 = 2048;
/// Earlier exchanges of a DM that Gemini sees, and how far back.
const HISTORY_TURNS: i64 = 3;
const HISTORY_MINUTES: i32 = 30;
/// The longest reply the bot sends, in characters (Telegram allows 4096).
pub const MAX_REPLY_CHARS: usize = 1000;

/// What the bot answers: one message, with the buttons the tools added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    pub buttons: Vec<Button>,
}

/// One turn's context: who asks, where, the names to keep from Gemini, and what the tools
/// produced along the way.
pub struct Turn {
    pub state: AppState,
    pub user_id: String,
    /// The chat the reply goes to.
    pub space_id: String,
    pub is_group: bool,
    /// The group's circle; None in a DM.
    pub circle_id: Option<String>,
    pub redactor: Redactor,
    /// Every Gemini and tool call, for `agent_turns.tool_calls`.
    pub calls: Vec<CallLog>,
    /// Buttons under the reply (links the tools add).
    pub buttons: Vec<Button>,
    /// Messages to other chats, such as meet-again's DMs, sent after the reply.
    pub sends: Vec<SendRequest>,
    /// The Gemini model, once called.
    pub model: Option<String>,
    /// Why the turn fell back, when it did.
    pub fallback_reason: Option<String>,
    /// Every tool's `say` line this turn, in order, each once: the facts the reply carries.
    pub says: Vec<String>,
    /// Outputs of tools that change something (meet-again, invite answers), so a turn never
    /// runs one twice: a repeated call gets the first output back.
    done: Vec<(Tool, tools::ToolOutput)>,
    /// (circle id, placeholder): the user's circles, whose names are free text.
    circles: Vec<(String, String)>,
    /// When the turn began: [`TURN_BUDGET`] counts from here.
    started: Instant,
}

impl Turn {
    /// A turn for `user_id`, with every name they could mention registered: their own (as
    /// `{you}`, with their Telegram first name and username), those of everyone who shares a
    /// circle with them (`{person_1}`, ...), each with its parts of three or more letters, their
    /// drop-off label (`{dropoff}`, shown as [`ordering::dropoff_shown`] says for this chat) and
    /// their circles' names (`{circle_1}`, ...). Words of the fixed text ([`common_words`]) are
    /// left alone.
    pub async fn new(
        state: &AppState,
        user_id: &str,
        space_id: &str,
        is_group: bool,
        circle_id: Option<&str>,
        sender_name: Option<&str>,
        sender_username: Option<&str>,
    ) -> ApiResult<Self> {
        let started = Instant::now();
        let fixed = common_words(&state.pool).await?;
        let mut redactor = Redactor::with_common_words(fixed.iter().map(String::as_str));
        let people: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT u.id, u.display_name, u.telegram_username FROM ferry.users u
              WHERE u.id = $1
                 OR u.id IN (SELECT o.user_id FROM ferry.circle_members m
                               JOIN ferry.circle_members o ON o.circle_id = m.circle_id
                              WHERE m.user_id = $1)
                 OR u.id IN (SELECT user_id FROM ferry.circle_members WHERE circle_id = $2)
              ORDER BY (u.id = $1) DESC, u.id",
        )
        .bind(user_id)
        .bind(circle_id)
        .fetch_all(&state.pool)
        .await?;
        let mut n = 0;
        for (id, display_name, username) in &people {
            if id == user_id {
                let mut names = vec![display_name.as_str()];
                names.extend(sender_name);
                redactor.person("you", display_name, &names);
                redactor.forbid(sender_username.unwrap_or_default());
            } else {
                n += 1;
                redactor.person(&format!("person_{n}"), display_name, &[display_name]);
            }
            if let Some(username) = username {
                redactor.forbid(username);
            }
        }
        if let Some(user) = load_user(&state.pool, user_id).await?
            && let Some(dropoff) = &user.profile.dropoff_default
        {
            let shown = ordering::dropoff_shown(
                &state.pool,
                dropoff.id.as_deref(),
                &dropoff.label,
                is_group,
            )
            .await?;
            redactor.private("dropoff", &shown, &[&dropoff.label]);
        }
        // Public communities last, so a user's own circles keep their placeholders.
        let circle_rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT c.id, c.name, c.emoji FROM ferry.circles c
              WHERE c.id IN (SELECT circle_id FROM ferry.circle_members WHERE user_id = $1)
                 OR c.id = $2
              ORDER BY c.is_public, c.id",
        )
        .bind(user_id)
        .bind(circle_id)
        .fetch_all(&state.pool)
        .await?;
        let mut circles = Vec::new();
        for (i, (id, name, emoji)) in circle_rows.iter().enumerate() {
            let shown = format!("{emoji} {name}");
            let placeholder =
                redactor.private(&format!("circle_{}", i + 1), &shown, &[name, &shown]);
            circles.push((id.clone(), placeholder));
        }
        Ok(Self {
            state: state.clone(),
            user_id: user_id.to_owned(),
            space_id: space_id.to_owned(),
            is_group,
            circle_id: circle_id.map(str::to_owned),
            redactor,
            calls: Vec::new(),
            buttons: Vec::new(),
            sends: Vec::new(),
            model: None,
            fallback_reason: None,
            says: Vec::new(),
            done: Vec::new(),
            circles,
            started,
        })
    }

    /// The placeholder of circle `id` (`{circle_N}`), registering it when the turn did not know it.
    pub fn circle(&mut self, id: &str, emoji: &str, name: &str) -> String {
        if let Some((_, placeholder)) = self.circles.iter().find(|(c, _)| c == id) {
            return placeholder.clone();
        }
        let shown = format!("{emoji} {name}");
        let label = format!("circle_{}", self.circles.len() + 1);
        let placeholder = self.redactor.private(&label, &shown, &[name, &shown]);
        self.circles.push((id.to_owned(), placeholder.clone()));
        placeholder
    }

    /// Adds `buttons` under the reply, each once.
    pub fn add_buttons(&mut self, buttons: &[Button]) {
        for button in buttons {
            if !self.buttons.contains(button) {
                self.buttons.push(button.clone());
            }
        }
    }

    /// Runs `tool` with `args`, logging the call and its latency. A tool that changes something
    /// runs at most once per turn.
    pub async fn call_tool(&mut self, tool: Tool, args: &Value) -> tools::ToolOutput {
        if let Some((_, output)) = self.done.iter().find(|(t, _)| *t == tool) {
            return output.clone();
        }
        let started = Instant::now();
        let output = tools::run(self, tool, args).await;
        if tool.changes_something() {
            self.done.push((tool, output.clone()));
        }
        if let Some(say) = output.say.as_deref().map(str::trim)
            && !say.is_empty()
            && !self.says.iter().any(|s| s == say)
        {
            self.says.push(say.to_owned());
        }
        self.calls.push(CallLog {
            name: tool.name().to_owned(),
            args: args.clone(),
            ok: output.ok,
            latency_ms: elapsed_ms(started),
            error: (!output.ok).then(|| "tool_error".to_owned()),
        });
        output
    }

    /// The finished reply: placeholders filled, cut to [`MAX_REPLY_CHARS`], with the tools'
    /// buttons.
    pub fn reply(&mut self, text: &str) -> Reply {
        let filled = self.redactor.fill(text.trim());
        let text = if filled.chars().count() > MAX_REPLY_CHARS {
            let cut: String = filled.chars().take(MAX_REPLY_CHARS - 1).collect();
            format!("{cut}…")
        } else {
            filled
        };
        Reply {
            text,
            buttons: std::mem::take(&mut self.buttons),
        }
    }
}

pub fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Answers `text` from the turn's user: through Gemini when it is configured and answers, else
/// through the fallback. Once Gemini failed earlier in the turn (the trip classifier), it is not
/// called again: two waits on a Gemini that does not answer would outlast the turn's 45 s. Gemini
/// gets what is left of the turn's [`TURN_BUDGET`] after the classifier, so a classifier that
/// answered late leaves the answer less time, never a fresh 25 s.
pub async fn answer(turn: &mut Turn, text: &str) -> Reply {
    if turn.fallback_reason.is_some() {
        return fallback::answer(turn, text).await;
    }
    let Some(gemini) = Gemini::for_app(&turn.state) else {
        turn.fallback_reason = Some("gemini_unconfigured".into());
        return fallback::answer(turn, text).await;
    };
    turn.model = Some(gemini.model().to_owned());
    let budget = TURN_BUDGET.saturating_sub(turn.started.elapsed());
    let reason = if budget < MIN_ANSWER_BUDGET {
        "turn_budget"
    } else {
        match tokio::time::timeout(budget, gemini_answer(turn, &gemini, text)).await {
            Ok(Ok(reply)) => return reply,
            Ok(Err(reason)) => reason,
            Err(_) => {
                // The call in flight was dropped before it could count. With at least a full
                // attempt's time it had its chance: count it as timed out for the breaker.
                if budget >= gemini::CALL_TIMEOUT {
                    turn.state
                        .gemini_breaker
                        .record::<()>(&Err(GeminiError::Timeout), &gemini);
                }
                "turn_budget"
            }
        }
    };
    tracing::warn!(reason, "agent turn fell back");
    turn.fallback_reason = Some(reason.to_owned());
    // A tool already answered (and may have changed something): say its lines rather than run
    // the fallback's tool again.
    if !turn.says.is_empty() {
        let lines = turn.says.join(" ");
        return turn.reply(&lines);
    }
    turn.buttons.clear();
    fallback::answer(turn, text).await
}

/// Words that state an order's fate; with digits and money, what Gemini may not say on its own.
const FACT_WORDS: [&str; 11] = [
    "placed",
    "cancelled",
    "canceled",
    "refund",
    "refunded",
    "charged",
    "dollars",
    "cents",
    "delivered",
    "picked",
    "arrived",
];

/// Phrases that state where an order is, as [`FACT_WORDS`] do.
const FACT_PHRASES: [&str; 1] = ["on its way"];

/// Whether `text` states a fact only code may state: a digit, a currency sign, a word of
/// [`FACT_WORDS`] or a phrase of [`FACT_PHRASES`]. Placeholders (`{invitee_1}`) are not facts.
pub fn states_facts(text: &str) -> bool {
    let mut bare = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        bare.push_str(&rest[..start]);
        match rest[start..].find('}') {
            Some(end) => rest = &rest[start + end + 1..],
            None => {
                rest = &rest[start..];
                break;
            }
        }
    }
    bare.push_str(rest);
    bare.chars()
        .any(|c| c.is_ascii_digit() || "$€£¢".contains(c))
        || bare
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| FACT_WORDS.contains(&w.to_lowercase().as_str()))
        || {
            let words: Vec<String> = bare
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .map(str::to_lowercase)
                .collect();
            let words = format!(" {} ", words.join(" "));
            FACT_PHRASES
                .iter()
                .any(|p| words.contains(&format!(" {p} ")))
        }
}

/// What the user gets once Gemini answered with `said` (empty when it gave no usable text):
/// - when tools spoke (`says`), their lines, or Gemini's words if they carry every line word for
///   word and add no fact of their own ([`states_facts`]);
/// - with no tool line, Gemini's words unless they state a fact; Err (the turn falls back) when
///   they do or when there are none.
pub fn final_text(says: &[String], said: &str) -> Result<String, &'static str> {
    let said = said.trim();
    if !says.is_empty() {
        let mut rest = said.to_owned();
        let carries = !said.is_empty()
            && says.iter().all(|say| {
                let found = rest.contains(say.as_str());
                rest = rest.replacen(say.as_str(), " ", 1);
                found
            });
        return Ok(if carries && !states_facts(&rest) {
            said.to_owned()
        } else {
            says.join(" ")
        });
    }
    if said.is_empty() {
        return Err("empty_answer");
    }
    if states_facts(said) {
        return Err("unsourced_facts");
    }
    Ok(said.to_owned())
}

/// The tool loop. Err carries a short reason for the log.
async fn gemini_answer(
    turn: &mut Turn,
    gemini: &Gemini,
    text: &str,
) -> Result<Reply, &'static str> {
    let system = system_prompt(turn).await;
    let mut contents = history(turn).await;
    contents.push(gemini::user_text(&turn.redactor.scrub(text)));
    let declarations = tools::declarations();

    for round in 0..=MAX_TOOL_ROUNDS {
        let mode = if round < MAX_TOOL_ROUNDS {
            ToolMode::Auto
        } else {
            ToolMode::None
        };
        let body =
            gemini::build_request(&system, &contents, &declarations, mode, MAX_OUTPUT_TOKENS);
        let sealed = turn.redactor.seal(body).map_err(|_| "privacy_guard")?;
        let started = Instant::now();
        let (result, attempts) = gemini.generate_counted(&sealed).await;
        let (ok, error) = match &result {
            Ok(_) => (true, None),
            Err(e) => (false, Some(e.label().to_owned())),
        };
        turn.calls.push(CallLog {
            name: "gemini".into(),
            args: json!({"round": round + 1, "attempts": attempts}),
            ok,
            latency_ms: elapsed_ms(started),
            error,
        });
        let generated = result.map_err(|e| e.label())?;

        if generated.calls.is_empty() || mode == ToolMode::None {
            // A reply cut short by the token cap is never sent.
            let truncated = generated.finish_reason.as_deref() == Some("MAX_TOKENS");
            let said = if truncated {
                ""
            } else {
                generated.text.as_str()
            };
            let text = final_text(&turn.says, said)?;
            return Ok(turn.reply(&text));
        }
        contents.push(generated.content.clone());
        let mut results = Vec::with_capacity(generated.calls.len());
        for call in &generated.calls {
            let result = match Tool::from_name(&call.name) {
                Some(tool) => turn.call_tool(tool, &call.args).await.result,
                None => json!({"error": format!("There is no tool named {}.", call.name)}),
            };
            results.push((call.clone(), result));
        }
        contents.push(gemini::function_responses(&results));
    }
    Err("too_many_rounds")
}

/// The system instruction: the rules, the chat, Chapel Hill's time and the user's saved
/// defaults, with no name in it.
async fn system_prompt(turn: &Turn) -> String {
    let now: String = sqlx::query_scalar(
        "SELECT to_char(now() AT TIME ZONE 'America/New_York', 'Dy, Mon FMDD, FMHH12:MI AM')",
    )
    .fetch_one(&turn.state.pool)
    .await
    .unwrap_or_else(|_| "unknown".into());
    let defaults = profile_summary(turn).await.unwrap_or_default();
    system_instruction(turn.is_group, &now, &defaults)
}

/// The rules Gemini gets, for a group or a DM, with Chapel Hill's time and the user's defaults
/// (already free of names).
pub fn system_instruction(is_group: bool, now: &str, defaults: &str) -> String {
    let chat = if is_group {
        "a group chat whose members are a Ferry circle; someone addressed you"
    } else {
        "a direct message with the user"
    };
    format!(
        "You are Ferry, a Telegram bot for UNC Chapel Hill students. Ferry turns trips people already \
make into food pickups for their group: a runner walks to a restaurant and brings back the group's \
orders, paid in the Ferry app, and every walking delivery saves the CO2 of a car trip.\n\
You are in {chat}. The user is {{you}}.\n\
People are never named to you. They appear as placeholders in braces, such as {{you}}, {{runner}} \
or {{invitee_1}}, and so do the user's drop-off ({{dropoff}}) and their groups ({{circle_1}}). Copy a \
placeholder exactly when you mention it, and never guess a name.\n\
Rules:\n\
- Reply in one or two short sentences of plain text, no markdown.\n\
- You cannot place or cancel an order: only the user's tap on its button does. confirm_order and \
cancel_order show those buttons; never say an order is placed or cancelled.\n\
- State facts (times, distances, ETAs, prices, totals, balances, counts, CO2) only from tool \
results in this conversation. Never invent or estimate them.\n\
- When a tool result has a \"say\" field, reply with that text exactly, unless the user asked \
something it does not answer.\n\
- Every time or location answer says how old the data is; the say lines already do.\n\
- Money and orders are handled by Ferry's code; never promise a charge, refund or delivery.\n\
- If you cannot help, say what you can do: food runs, ETAs, menus and what's open, the user's \
Ferry balance, CO2 saved, and inviting their last group to meet again.\n\
Now in Chapel Hill: {now}.\n{defaults}"
    )
}

/// The user's questionnaire answers as defaults for the agent, without their name.
async fn profile_summary(turn: &Turn) -> Option<String> {
    let user = load_user(&turn.state.pool, &turn.user_id).await.ok()??;
    let p = &user.profile;
    let mut lines = Vec::new();
    if p.dropoff_default.is_some() {
        // Free text: Gemini gets the placeholder (registered in `Turn::new`).
        lines.push(format!("default drop-off: {}", ordering::DROPOFF));
    }
    if !p.diet.is_empty() {
        lines.push(format!("diet: {}", turn.redactor.scrub(&p.diet.join(", "))));
    }
    if !p.avoid.is_empty() {
        lines.push(format!(
            "avoids: {}",
            turn.redactor.scrub(&p.avoid.join(", "))
        ));
    }
    if !p.favorite_cuisines.is_empty() {
        lines.push(format!(
            "favorite cuisines: {}",
            turn.redactor.scrub(&p.favorite_cuisines.join(", "))
        ));
    }
    if !p.favorite_restaurant_ids.is_empty() {
        lines.push(format!(
            "favorite restaurants: {}",
            p.favorite_restaurant_ids.join(", ")
        ));
    }
    lines.push(format!(
        "default tip: {}",
        tools::dollars(p.default_tip_cents)
    ));
    lines.push(format!("walking pace: {}", p.walking_pace.as_str()));
    Some(format!(
        "The user's saved defaults (use them for suggestions): {}.",
        lines.join("; ")
    ))
}

/// Day and month names: the instruction's clock shows them.
const CALENDAR: &str = "Monday Mon Tuesday Tue Wednesday Wed Thursday Thu Friday Fri Saturday Sat \
Sunday Sun January Jan February Feb March Mar April Apr May June Jun July Jul August Aug September \
Sep Sept October Oct November Nov December Dec AM PM";

/// The text every request may carry whatever the user says: the instructions, the tools'
/// declarations, the trip classifier's instruction, the help line, the calendar, and the
/// catalog's names (restaurants, cuisines, menu items and their options, choices and tags). A
/// name part made of these words is not a name to Gemini ([`Redactor::with_common_words`]).
pub async fn common_words(pool: &sqlx::PgPool) -> ApiResult<Vec<String>> {
    let mut fixed = vec![
        system_instruction(true, "", ""),
        system_instruction(false, "", ""),
        serde_json::to_string(&tools::declarations()).unwrap_or_default(),
        trips::classifier_instruction(""),
        fallback::HELP.to_owned(),
        CALENDAR.to_owned(),
    ];
    let catalog: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM ferry.restaurants
         UNION SELECT unnest(cuisine) FROM ferry.restaurants
         UNION SELECT name FROM ferry.menu_items
         UNION SELECT unnest(tags) FROM ferry.menu_items
         UNION SELECT name FROM ferry.item_options
         UNION SELECT name FROM ferry.item_choices",
    )
    .fetch_all(pool)
    .await?;
    fixed.extend(catalog);
    Ok(fixed)
}

/// A DM's last few exchanges within the last half hour, scrubbed like the new message with this
/// turn's names. None in groups: the bot answers each addressed message on its own there.
async fn history(turn: &Turn) -> Vec<Value> {
    if turn.is_group {
        return Vec::new();
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT inbound_text, reply_text FROM (
             SELECT inbound_text, reply_text, id FROM ferry.agent_turns
              WHERE space_id = $1 AND user_id = $2 AND reply_text IS NOT NULL
                AND inbound_text <> ''
                AND created_at > now() - make_interval(mins => $3)
              ORDER BY id DESC LIMIT $4
         ) recent ORDER BY id",
    )
    .bind(&turn.space_id)
    .bind(&turn.user_id)
    .bind(HISTORY_MINUTES)
    .bind(HISTORY_TURNS)
    .fetch_all(&turn.state.pool)
    .await
    .unwrap_or_default();
    rows.iter()
        .flat_map(|(said, answered)| {
            [
                gemini::user_text(&turn.redactor.scrub(said)),
                gemini::model_text(&turn.redactor.scrub(answered)),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(says: &[&str]) -> Vec<String> {
        says.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn tool_lines_win_over_geminis_words() {
        let eta = lines(&["{runner} is 22 min away (1.3 mi), seen 20 s ago."]);
        // Gemini's own numbers never reach the user.
        assert_eq!(
            final_text(&eta, "{runner} is 1 min away (0.1 mi), seen 1 s ago.").unwrap(),
            eta[0]
        );
        assert_eq!(final_text(&eta, "Here is the menu.").unwrap(), eta[0]);
        assert_eq!(final_text(&eta, "").unwrap(), eta[0]);
        // Gemini's words pass when they carry the line word for word and add no fact.
        let kind = format!("Good news! {} Enjoy.", eta[0]);
        assert_eq!(final_text(&eta, &kind).unwrap(), kind);
        let extra = format!("{} That's 5 min faster than usual.", eta[0]);
        assert_eq!(final_text(&eta, &extra).unwrap(), eta[0]);
        // Several tools: every line, in order.
        let two = lines(&["Your Ferry balance is $50.00.", "Open now: Cosmic Cantina."]);
        assert_eq!(
            final_text(&two, "Your Ferry balance is $50.00.").unwrap(),
            "Your Ferry balance is $50.00. Open now: Cosmic Cantina."
        );
    }

    #[test]
    fn without_a_tool_gemini_states_no_fact() {
        assert_eq!(
            final_text(&[], "Your total is $1.00."),
            Err("unsourced_facts")
        );
        assert_eq!(
            final_text(&[], "Maya is 3 min away"),
            Err("unsourced_facts")
        );
        assert_eq!(
            final_text(&[], "Your order is placed!"),
            Err("unsourced_facts")
        );
        assert_eq!(final_text(&[], ""), Err("empty_answer"));
        assert_eq!(
            final_text(&[], "Hi! Ask me what's open or how far your food is.").unwrap(),
            "Hi! Ask me what's open or how far your food is."
        );
        // Placeholders are not facts.
        assert!(!states_facts("Say hi to {invitee_1} and {circle_2}."));
        assert!(states_facts("Ten bucks is $10"));
        assert!(states_facts("It was CANCELLED"));
        // Round 2's MINOR 2: what Gemini said on its own in the review.
        for said in [
            "Maya already delivered it.",
            "Your refund is on its way too.",
            "Good news: your burrito is on its way and almost there!",
            "Maya picked it up.",
            "Your food has arrived.",
            "Your burrito is ON ITS\nWAY",
        ] {
            assert!(states_facts(said), "{said}");
        }
        assert!(!states_facts("Have a good night on your way home."));
    }

    #[test]
    fn the_fixed_text_passes_the_seal() {
        let fixed = [
            system_instruction(true, "Sat, Oct 11, 7:42 PM", ""),
            system_instruction(false, "", ""),
            serde_json::to_string(&tools::declarations()).unwrap(),
            trips::classifier_instruction("- rst_cosmic: Cosmic Cantina (mexican)"),
            fallback::HELP.to_owned(),
        ];
        let body = gemini::build_request(
            &fixed.join("\n"),
            &[gemini::user_text(
                "{you} asks: how long until my food gets here?",
            )],
            &tools::declarations(),
            ToolMode::Auto,
            MAX_OUTPUT_TOKENS,
        );
        assert!(Redactor::new().seal(body.clone()).is_ok());
        // A member named after a word of that text refuses nothing.
        let mut r = Redactor::with_common_words(fixed.iter().map(String::as_str));
        r.person("person_1", "Runner", &[]);
        r.person("person_2", "Place Order", &[]);
        assert!(r.seal(body).is_ok());
    }
}
