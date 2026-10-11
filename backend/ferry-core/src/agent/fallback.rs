//! The deterministic answer: what the bot says without Gemini (no key, an outage, a refused
//! payload, a turn out of time). Keywords pick one tool, and its `say` line is the reply, so the
//! bot still answers simply with facts from code (spec section 3, Resilience).
//!
//! No keyword moves money. "place it" or "confirm" shows the user's draft again with its Place
//! order button, and "cancel my order" shows what can be cancelled, each with its own button
//! (the confirm_order and cancel_order tools). "don't cancel ..." asks for the order's status
//! instead.

use serde_json::{Value, json};

use super::meet;
use super::ordering;
use super::patterns::{self, MeetMention, PlaceMatch, match_place};
use super::tools::Tool;
use super::turn::{Reply, Turn};

/// What a message asks for, as far as keywords tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Eta,
    /// A meet-again with a place or a time ("anyone want to go to Cosmic at 7?"): asked once
    /// before anything is sent.
    MeetPlace(MeetMention),
    MeetAgain,
    Wallet,
    Impact,
    /// A restaurant's menu: the words after "menu", or the whole text, to match a name.
    Menu(String),
    Restaurants,
    Profile,
    AnswerInvite(bool),
    CancelOrder,
    ConfirmOrder,
    OrderStatus,
    OpenRuns,
    Help,
}

/// What the bot says when it can't tell what is asked.
pub const HELP: &str = "I can tell you how far away your food is, what's open and what's on the menu, your Ferry balance and the CO2 you've saved, or invite your last group to meet up again.";

fn has_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| text.contains(n))
}

/// Whether `t` (lowercase) negates what it asks: "don't", "do not", "not", "never", "can't",
/// "won't".
fn negated(t: &str) -> bool {
    let t = t.replace('’', "'");
    t.split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .any(|w| {
            matches!(
                w,
                "don't"
                    | "dont"
                    | "not"
                    | "never"
                    | "shouldn't"
                    | "wouldn't"
                    | "can't"
                    | "cant"
                    | "cannot"
                    | "won't"
                    | "wont"
            )
        })
}

/// The intent of `text`, from keywords. Checked in order, so "how long" wins over "food".
pub fn route(text: &str) -> Intent {
    let t = text.to_lowercase();
    if t.contains("cancel") && t.contains("order") {
        return if negated(&t) {
            Intent::OrderStatus
        } else {
            Intent::CancelOrder
        };
    }
    if has_any(
        &t,
        &[
            "place it",
            "place my order",
            "place the order",
            "confirm",
            "yes order",
            "order it",
        ],
    ) {
        return Intent::ConfirmOrder;
    }
    if has_any(
        &t,
        &[
            "how long",
            "eta",
            "where is my",
            "where's my",
            "wheres my",
            "how far",
            "get here",
            "gets here",
            "on the way",
            "when will my",
            "arrive",
        ],
    ) {
        return Intent::Eta;
    }
    if let Some(mention) = patterns::meet_mention(text) {
        return Intent::MeetPlace(mention);
    }
    if t.contains("meet") && has_any(&t, &["last time", "again", "hung out", "hang out"]) {
        return Intent::MeetAgain;
    }
    if has_any(
        &t,
        &[
            "balance",
            "wallet",
            "owe",
            "how much money",
            "my money",
            "funds",
        ],
    ) {
        return Intent::Wallet;
    }
    if has_any(&t, &["co2", "carbon", "impact", "emission", "saved"]) {
        return Intent::Impact;
    }
    if has_any(&t, &["my order", "order status", "status of my"]) {
        return Intent::OrderStatus;
    }
    if has_any(
        &t,
        &[
            "open runs",
            "who's going",
            "whos going",
            "anyone going",
            "any runs",
            "taking orders",
        ],
    ) {
        return Intent::OpenRuns;
    }
    if let Some(pos) = t.find("menu") {
        let after = t[pos + 4..]
            .trim_start_matches([' ', ':'])
            .trim_start_matches("for ")
            .trim_start_matches("at ")
            .trim_start_matches("from ");
        let before = t[..pos].trim();
        let words = if after.trim().is_empty() {
            before
        } else {
            after
        };
        return Intent::Menu(words.trim_matches(['?', '!', '.', ' ']).to_owned());
    }
    // "sorry, I can't accept" accepts nothing. Only the accept is checked: "can't make it" is
    // itself a decline.
    if !negated(&t)
        && has_any(
            &t,
            &[
                "count me in",
                "i'm in",
                "im in",
                "accept",
                "i'll come",
                "ill come",
            ],
        )
    {
        return Intent::AnswerInvite(true);
    }
    if has_any(
        &t,
        &[
            "decline",
            "can't make it",
            "cant make it",
            "i'll pass",
            "ill pass",
        ],
    ) {
        return Intent::AnswerInvite(false);
    }
    if has_any(
        &t,
        &[
            "what's open",
            "whats open",
            "what is open",
            "open now",
            "restaurant",
            "hungry",
            "places",
            "where can",
            "food",
        ],
    ) {
        return Intent::Restaurants;
    }
    if has_any(
        &t,
        &[
            "my profile",
            "my defaults",
            "preferences",
            "my settings",
            "drop-off",
            "dropoff",
        ],
    ) {
        return Intent::Profile;
    }
    Intent::Help
}

/// Answers `text` with one tool, chosen by [`route`].
pub async fn answer(turn: &mut Turn, text: &str) -> Reply {
    // An order in words on a run the user can order on comes first: "chicken burrito no beans".
    let ordering_run = ordering::run_for_text(
        &turn.state.pool,
        &turn.user_id,
        turn.circle_id.as_deref(),
        text,
    )
    .await
    .ok()
    .flatten();
    let intent = route(text);
    if ordering_run.is_some()
        && matches!(intent, Intent::Help | Intent::Restaurants | Intent::Menu(_))
    {
        let output = turn
            .call_tool(Tool::DraftOrderFromText, &json!({"text": text}))
            .await;
        let say = output.say.unwrap_or_else(|| HELP.to_owned());
        return turn.reply(&say);
    }
    let (tool, args): (Tool, Value) = match intent {
        Intent::Eta => (Tool::GetEta, json!({})),
        Intent::MeetPlace(mention) => return meet::ask(turn, &mention).await,
        Intent::MeetAgain => (Tool::StartMeetAgain, json!({})),
        Intent::Wallet => (Tool::GetWallet, json!({})),
        Intent::Impact => (Tool::ExplainImpact, json!({})),
        Intent::Profile => (Tool::GetProfile, json!({})),
        Intent::AnswerInvite(accept) => (Tool::RespondInvite, json!({"accept": accept})),
        Intent::CancelOrder => (Tool::CancelOrder, json!({})),
        Intent::ConfirmOrder => (Tool::ConfirmOrder, json!({})),
        Intent::OrderStatus => (Tool::GetOrderStatus, json!({})),
        Intent::OpenRuns => (Tool::ListOpenRuns, json!({})),
        Intent::Restaurants => (Tool::ListRestaurants, json!({"open_now": true})),
        Intent::Menu(words) => match menu_target(turn, &words).await {
            Some(restaurant) => (Tool::GetMenu, json!({"restaurant": restaurant})),
            None => (Tool::ListRestaurants, json!({"open_now": true})),
        },
        Intent::Help => return turn.reply(HELP),
    };
    let output = turn.call_tool(tool, &args).await;
    let say = output.say.unwrap_or_else(|| HELP.to_owned());
    turn.reply(&say)
}

/// The restaurant `words` name, if exactly one.
async fn menu_target(turn: &Turn, words: &str) -> Option<String> {
    let all: Vec<(String, String)> =
        sqlx::query_as("SELECT id, name FROM ferry.restaurants ORDER BY name, id")
            .fetch_all(&turn.state.pool)
            .await
            .ok()?;
    match match_place(words, &all) {
        PlaceMatch::One(id) => Some(id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        let cases = [
            ("how long until my food gets here?", Intent::Eta),
            ("where's my burrito", Intent::Eta),
            (
                "let's meet with the people I hung out with last time",
                Intent::MeetAgain,
            ),
            ("what do I owe", Intent::Wallet),
            ("how much CO2 have we saved", Intent::Impact),
            ("cosmic menu", Intent::Menu("cosmic".into())),
            ("menu for sup dogs?", Intent::Menu("sup dogs".into())),
            ("what's open on Franklin", Intent::Restaurants),
            ("count me in", Intent::AnswerInvite(true)),
            // Round 2's MINOR 3: a negated accept accepts nothing.
            ("sorry, I can't accept, busy tonight", Intent::Help),
            ("sorry, I can’t accept", Intent::Help),
            ("I won't accept", Intent::Help),
            ("don't count me in", Intent::Help),
            ("can't make it", Intent::AnswerInvite(false)),
            ("what are my defaults", Intent::Profile),
            ("cancel my order", Intent::CancelOrder),
            ("please don't cancel my order", Intent::OrderStatus),
            ("I can't cancel my order", Intent::OrderStatus),
            ("yes place it", Intent::ConfirmOrder),
            // Shows the draft with its button; it never places it.
            ("wait, don't place it yet", Intent::ConfirmOrder),
            ("what's the status of my order?", Intent::OrderStatus),
            ("is anyone going anywhere?", Intent::OpenRuns),
            ("hello", Intent::Help),
        ];
        for (text, intent) in cases {
            assert_eq!(route(text), intent, "{text}");
        }
        // A meet-again with a place or a time is asked about before anything is sent.
        let cases = [
            ("anyone want to go to Cosmic at 7?", Some("Cosmic")),
            (
                "ask the people from last time if they want Cosmic at 7",
                Some("Cosmic"),
            ),
            (
                "anyone want to get Sup Dogs at 8 tomorrow?",
                Some("Sup Dogs"),
            ),
            ("anyone want to get dinner at 7?", None),
        ];
        for (text, place) in cases {
            match route(text) {
                Intent::MeetPlace(mention) => assert_eq!(mention.place.as_deref(), place, "{text}"),
                other => panic!("{text}: {other:?}"),
            }
        }
    }
}
