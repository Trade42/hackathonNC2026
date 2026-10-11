//! PR #22's review, BLOCKER 2 and MINORs 5, 6 and 7, through `POST /internal/messages/inbound`,
//! a stub sidecar and a stub Gemini (never the real one): a free-text drop-off label, a circle
//! named after a person, other members' name parts, the scrub gaps (a handle in parentheses, an
//! address with a full stop, a letter whose lowercase is longer, the input that panicked) and a
//! member whose name holds a word of the menu. Each reaches Gemini only as a placeholder or a
//! scrub mark, and the user gets the real words back from code. Every test ends with the privacy
//! scan of what reached Gemini.
mod agent_support;
mod common;

use std::time::Duration;

use agent_support::{
    DANA_TG, DANA_USERNAME, GROUP_ID, JUDGE_TG, JUDGE_USERNAME, STUB_INVITE_LINK, TestApp, app,
    call_reply, dm, group_message, inbound, json_reply, map_demo_group, seed_demo, text_reply,
};
use ferry_core::agent::dispatch::SNAG;
use serde_json::{Value, json};
use sqlx::PgPool;

/// The review's drop-off label: free text that holds a name and a street address.
const ZELDA: &str = "Zelda's place, 123 Main Street";

/// The judge's draft for "chicken burrito no beans" on run_demo, up to the drop-off: $8.99, tax
/// 7.5 % (67 c), the $1.50 Ferry fee and the judge's default $2.00 tip.
const DRAFT_PRICED: &str = "Chicken Burrito (No beans) from Cosmic Cantina: $8.99 + tax $0.67 + Ferry fee $1.50 + tip $2.00 = $13.16";

/// The draft line ending at the drop-off `shown`.
fn draft_line(shown: &str) -> String {
    format!("{DRAFT_PRICED}, to {shown}. Place the order?")
}

fn text(sent: &Value) -> &str {
    sent["text"].as_str().unwrap()
}

fn judge_dm(said: &str) -> Value {
    dm(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", said)
}

fn dana_dm(said: &str) -> Value {
    dm(DANA_TG, Some(DANA_USERNAME), "Dana", said)
}

/// The demo with money (the judge has $50) and the demo group mapped to cir_hinton4.
async fn seed_with_money(pool: &PgPool) {
    seed_demo(pool).await;
    map_demo_group(pool).await;
    ferry_core::payments::seed_demo_balances(pool)
        .await
        .unwrap();
}

/// The judge's saved drop-off becomes `label`, at the seeded spot (Fetzer Gym's coordinates),
/// with no note and the place `place_id` (None: a spot that is not a known place).
async fn judge_dropoff(pool: &PgPool, place_id: Option<&str>, label: &str) {
    let done = sqlx::query(
        "UPDATE ferry.profiles SET dropoff_place_id = $1, dropoff_label = $2, dropoff_note = NULL
          WHERE user_id = 'usr_judge'",
    )
    .bind(place_id)
    .bind(label)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(done.rows_affected(), 1, "the judge has no profile");
}

async fn set_display_name(pool: &PgPool, user_id: &str, name: &str) {
    sqlx::query("UPDATE ferry.users SET display_name = $2 WHERE id = $1")
        .bind(user_id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
}

/// Gemini for these tests: a trip classification says no trip; once a tool answered, Gemini
/// repeats its `say` line; otherwise `first` answers from the newest user text, as Gemini got it.
fn gemini_with(app: &TestApp, first: impl Fn(&str) -> Value + Send + Sync + 'static) {
    app.gemini.respond_with(move |body| {
        if body["generationConfig"]["responseMimeType"] == "application/json" {
            return (
                200,
                json_reply(&json!({"is_trip": false, "restaurant_id": null})),
            );
        }
        let last = body["contents"]
            .as_array()
            .and_then(|c| c.last())
            .cloned()
            .unwrap_or_default();
        if let Some(response) = last["parts"][0].get("functionResponse") {
            return (
                200,
                text_reply(response["response"]["say"].as_str().unwrap_or("done")),
            );
        }
        (200, first(last["parts"][0]["text"].as_str().unwrap_or("")))
    });
}

/// The newest user text of Gemini request `i`: that turn's message, after any replayed history.
fn last_user_text(app: &TestApp, i: usize) -> String {
    let requests = app.gemini.requests();
    let request = requests
        .get(i)
        .unwrap_or_else(|| panic!("no Gemini request {i}; there are {}", requests.len()));
    let last = request.body["contents"]
        .as_array()
        .and_then(|c| c.last())
        .unwrap_or_else(|| panic!("Gemini request {i} has no contents"));
    assert_eq!(last["role"], "user", "{last}");
    last["parts"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("Gemini request {i} ends with no user text: {last}"))
        .to_owned()
}

/// The texts of the `model` turns replayed in Gemini request `i` (a DM's history).
fn model_texts(app: &TestApp, i: usize) -> Vec<String> {
    let requests = app.gemini.requests();
    requests[i].body["contents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["role"] == "model")
        .flat_map(|c| c["parts"].as_array().cloned().unwrap_or_default())
        .filter_map(|p| p["text"].as_str().map(str::to_owned))
        .collect()
}

/// Every string in `value`, decoded, keys included.
fn strings_in(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| strings_in(v, out)),
        Value::Object(map) => {
            for (key, v) in map {
                out.push(key.clone());
                strings_in(v, out);
            }
        }
        _ => {}
    }
}

/// Asserts no recorded Gemini request holds any of `terms`, in any case, in its serialized body
/// or in any decoded string. Names the request and the term, never the body.
fn assert_never_sent(app: &TestApp, terms: &[&str]) {
    for (i, request) in app.gemini.requests().iter().enumerate() {
        let mut strings = vec![request.body.to_string()];
        strings_in(&request.body, &mut strings);
        for term in terms {
            let term_lower = term.to_lowercase();
            assert!(
                !strings
                    .iter()
                    .any(|s| s.to_lowercase().contains(&term_lower)),
                "Gemini request {i} holds {term:?}"
            );
        }
    }
}

/// A finished turn: its inbound text, reply and error.
type TurnRow = (String, Option<String>, Option<String>);

/// Waits until `count` turns are finished (a turn's row is written after its sends), and returns
/// every finished turn, oldest first.
async fn finished_turns(pool: &PgPool, count: usize) -> Vec<TurnRow> {
    for _ in 0..500 {
        let rows: Vec<TurnRow> = sqlx::query_as(
            "SELECT inbound_text, reply_text, error FROM ferry.agent_turns
              WHERE latency_ms IS NOT NULL ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        if rows.len() >= count {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("fewer than {count} agent turns finished");
}

/// Asserts every finished turn ran without a fallback, a privacy refusal or a panic.
fn assert_no_errors(turns: &[TurnRow]) {
    for (said, _, error) in turns {
        assert_eq!(error.as_deref(), None, "the turn for {said:?} failed");
    }
}

/// BLOCKER 2a: the judge's drop-off is the free-text label "Zelda's place, 123 Main Street", with
/// no place. In a DM, Gemini's draft and confirm results, its system instruction and the replayed
/// history name it only as `{dropoff}`, and the judge's DM gets the label back from code.
#[sqlx::test(migrations = "../migrations")]
async fn a_free_text_dropoff_reaches_gemini_as_a_placeholder_and_the_judge_as_written(
    pool: PgPool,
) {
    seed_with_money(&pool).await;
    judge_dropoff(&pool, None, ZELDA).await;
    let app = app(pool.clone(), true, &[]).await;
    gemini_with(&app, |_| {
        call_reply(
            "draft_order_from_text",
            json!({"text": "chicken burrito no beans"}),
        )
    });

    inbound(&app, &judge_dm("chicken burrito no beans")).await;
    let sent = app.sidecar.wait_for(1).await;
    assert_eq!(sent[0].body["space_id"], JUDGE_TG);
    assert_eq!(text(&sent[0].body), draft_line(ZELDA));
    let results = app.gemini.function_responses(1);
    assert_eq!(results[0].0, "draft_order_from_text");
    assert_eq!(results[0].1["total"], "$13.16");
    assert_eq!(results[0].1["say"], draft_line("{dropoff}"));
    let system = app.gemini.requests()[0].body["systemInstruction"].to_string();
    assert!(system.contains("default drop-off: {dropoff}"), "{system}");
    finished_turns(&pool, 1).await;

    // "yes place it": confirm_order shows the draft again, with the placeholder too, and the
    // first reply comes back in the history with the label scrubbed to `{dropoff}`.
    gemini_with(&app, |_| call_reply("confirm_order", json!({})));
    inbound(&app, &judge_dm("yes place it")).await;
    let sent = app.sidecar.wait_for(2).await;
    assert_eq!(text(&sent[1].body), draft_line(ZELDA));
    let last = app.gemini.request_count() - 1;
    let results = app.gemini.function_responses(last);
    assert_eq!(results[0].0, "confirm_order");
    assert_eq!(results[0].1["say"], draft_line("{dropoff}"));
    let replayed = model_texts(&app, last);
    assert!(
        replayed.iter().any(|t| *t == draft_line("{dropoff}")),
        "{replayed:?}"
    );
    assert_no_errors(&finished_turns(&pool, 2).await);
    assert_never_sent(&app, &["Zelda", "Main Street", "123 Main"]);
    app.gemini.assert_private();
}

/// BLOCKER 2a, with Gemini off: the fallback's DM draft names the judge's own label, and nothing
/// reaches Gemini.
#[sqlx::test(migrations = "../migrations")]
async fn without_gemini_a_dm_draft_names_the_free_text_dropoff(pool: PgPool) {
    seed_with_money(&pool).await;
    judge_dropoff(&pool, None, ZELDA).await;
    let app = app(pool.clone(), false, &[]).await;

    inbound(&app, &judge_dm("chicken burrito no beans")).await;
    let sent = app.sidecar.wait_for(1).await;
    assert_eq!(sent[0].body["space_id"], JUDGE_TG);
    assert_eq!(text(&sent[0].body), draft_line(ZELDA));
    assert_eq!(app.gemini.request_count(), 0);
    app.gemini.assert_private();
}

/// MINOR 5: in the group, a drop-off that is not a known place shows as "your drop-off", never
/// the free-text label; the draft still keeps the label for the runner. No Gemini call is made.
#[sqlx::test(migrations = "../migrations")]
async fn a_free_text_dropoff_shows_as_your_drop_off_in_the_group(pool: PgPool) {
    seed_with_money(&pool).await;
    judge_dropoff(&pool, None, ZELDA).await;
    let app = app(pool.clone(), true, &[]).await;

    let order = group_message(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "chicken burrito no beans",
    );
    assert!(inbound(&app, &order).await.is_empty());
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    let line = text(&sent[0].body);
    assert_eq!(sent[0].body["space_id"], GROUP_ID);
    assert_eq!(line, format!("Jordan: {}", draft_line("your drop-off")));
    assert!(
        !line.contains("Zelda") && !line.contains("Main Street"),
        "{line}"
    );
    let kept: String =
        sqlx::query_scalar("SELECT request->'dropoff'->>'label' FROM ferry.order_drafts")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(kept, ZELDA);
    assert_eq!(app.gemini.request_count(), 0);
    app.gemini.assert_private();
}

/// MINOR 5: a known place shows in the group by its own name (`ferry.places`), even when the
/// user's label for it is private text; the user's own DM shows their label.
#[sqlx::test(migrations = "../migrations")]
async fn a_known_place_shows_its_own_name_in_the_group_and_the_label_only_in_a_dm(pool: PgPool) {
    seed_with_money(&pool).await;
    judge_dropoff(&pool, Some("place_fetzer"), ZELDA).await;
    let app = app(pool.clone(), false, &[]).await;

    let order = group_message(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "chicken burrito no beans",
    );
    assert!(inbound(&app, &order).await.is_empty());
    let sent = app.sidecar.wait_for(1).await;
    assert_eq!(sent[0].body["space_id"], GROUP_ID);
    assert_eq!(
        text(&sent[0].body),
        format!("Jordan: {}", draft_line("Fetzer Gym (HackNC)"))
    );

    inbound(&app, &judge_dm("chicken burrito no beans")).await;
    let sent = app.sidecar.wait_for(2).await;
    assert_eq!(sent[1].body["space_id"], JUDGE_TG);
    assert_eq!(text(&sent[1].body), draft_line(ZELDA));
    app.gemini.assert_private();
}

/// BLOCKER 2b: the circle is named "Sam Smith's apt" (circle names come from chat titles). Dana
/// asks to meet there (meet-place: a typed place goes to Gemini scrubbed); her meet-again reply
/// names it from code, and her next DM, "thanks!", replays that reply to Gemini with the circle as
/// `{circle_1}`, never its name.
#[sqlx::test(migrations = "../migrations")]
async fn a_circles_name_reaches_gemini_only_as_a_placeholder_in_dm_history(pool: PgPool) {
    seed_demo(&pool).await;
    map_demo_group(&pool).await;
    sqlx::query(
        "UPDATE ferry.message_spaces SET bot_is_admin = true, invite_url = $2 WHERE space_id = $1",
    )
    .bind(GROUP_ID)
    .bind(STUB_INVITE_LINK)
    .execute(&pool)
    .await
    .unwrap();
    let emoji: String = sqlx::query_scalar(
        "UPDATE ferry.circles SET name = 'Sam Smith''s apt' WHERE id = 'cir_hinton4' RETURNING emoji",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let app = app(pool.clone(), true, &[]).await;
    gemini_with(&app, |_| {
        call_reply(
            "start_meet_again",
            json!({"place": "{circle_1}", "when": "at 7pm"}),
        )
    });

    inbound(
        &app,
        &dana_dm("anyone want to go to Sam Smith's apt at 7pm?"),
    )
    .await;
    app.sidecar.wait_for(3).await;
    let reply = text(&app.sidecar.sent_to(DANA_TG)[0]).to_owned();
    assert!(
        reply.starts_with(&format!(
            "Pinged Jordan, Leo and Maya: {emoji} Sam Smith's apt at 7:00 PM"
        )),
        "{reply}"
    );
    assert!(reply.ends_with("Leo gets it in the app only."), "{reply}");
    assert!(
        text(&app.sidecar.sent_to(JUDGE_TG)[0]).starts_with(&format!(
            "Dana wants to go to {emoji} Sam Smith's apt at 7:00 PM"
        )),
    );
    let results = app.gemini.function_responses(1);
    assert_eq!(results[0].0, "start_meet_again");
    assert!(
        results[0].1["plan"]
            .as_str()
            .unwrap()
            .starts_with("{circle_1} at 7:00 PM"),
        "{}",
        results[0].1
    );
    // The next turn's history holds this reply only once its row is finished.
    finished_turns(&pool, 1).await;

    app.gemini
        .respond_with(|_| (200, text_reply("You're welcome!")));
    inbound(&app, &dana_dm("thanks!")).await;
    let sent = app.sidecar.wait_for(4).await;
    assert_eq!(sent[3].body["space_id"], DANA_TG);
    assert_eq!(text(&sent[3].body), "You're welcome!");
    let last = app.gemini.request_count() - 1;
    assert_eq!(last_user_text(&app, last), "thanks!");
    let replayed = model_texts(&app, last);
    assert!(
        replayed
            .iter()
            .any(|t| t.contains(": {circle_1} at 7:00 PM") && t.starts_with("Pinged {person_")),
        "{replayed:?}"
    );
    assert_no_errors(&finished_turns(&pool, 2).await);
    assert_never_sent(&app, &["Sam Smith", "Smith"]);
    app.gemini.assert_private();
}

/// BLOCKER 2c: Maya's display name is "Maya Lopez". "is maya bringing my food?" and "LOPEZ said 5
/// min" reach Gemini with each part as her placeholder (the same one), and code fills her full
/// name back into the judge's reply.
#[sqlx::test(migrations = "../migrations")]
async fn each_part_of_another_members_name_reaches_gemini_as_their_placeholder(pool: PgPool) {
    seed_demo(&pool).await;
    set_display_name(&pool, "usr_maya", "Maya Lopez").await;
    let app = app(pool.clone(), true, &[]).await;
    // Gemini names the first person the message named, by the placeholder it got.
    gemini_with(&app, |said| {
        let placeholder = said
            .find('{')
            .and_then(|start| {
                said[start..]
                    .find('}')
                    .map(|end| &said[start..=start + end])
            })
            .unwrap_or("nobody");
        text_reply(&format!("I'll ask {placeholder} for you."))
    });

    inbound(&app, &judge_dm("is maya bringing my food?")).await;
    let sent = app.sidecar.wait_for(1).await;
    let first = last_user_text(&app, 0);
    let maya = first
        .strip_prefix("is ")
        .and_then(|rest| rest.strip_suffix(" bringing my food?"))
        .unwrap_or_else(|| panic!("not scrubbed: {first:?}"))
        .to_owned();
    assert!(
        maya.starts_with("{person_") && maya.ends_with('}'),
        "{first:?}"
    );
    assert_eq!(text(&sent[0].body), "I'll ask Maya Lopez for you.");
    finished_turns(&pool, 1).await;

    inbound(&app, &judge_dm("LOPEZ said 5 min")).await;
    let sent = app.sidecar.wait_for(2).await;
    let last = app.gemini.request_count() - 1;
    assert_eq!(last_user_text(&app, last), format!("{maya} said 5 min"));
    assert_eq!(text(&sent[1].body), "I'll ask Maya Lopez for you.");
    assert_no_errors(&finished_turns(&pool, 2).await);
    assert_never_sent(&app, &["Maya", "Lopez"]);
    app.gemini.assert_private();
}

/// BLOCKER 2d and MINOR 6: a handle inside parentheses, an address ending in a full stop, and a
/// letter whose lowercase is longer (`İ`) ahead of the sender's own name, behind a line break,
/// each reach Gemini scrubbed. "İ maya’Å", which panicked the turn, gets a reply and a finished
/// turn row with no error.
#[sqlx::test(migrations = "../migrations")]
async fn the_reviews_scrub_gaps_reach_gemini_scrubbed_and_never_panic(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    gemini_with(&app, |_| text_reply("Got it."));

    let cases: [(Value, &str, &str); 3] = [
        (
            judge_dm("ping my roommate (@sam_k99) about dinner"),
            JUDGE_TG,
            "ping my roommate (@someone) about dinner",
        ),
        (
            judge_dm("bring it to 412 Pittsboro Street."),
            JUDGE_TG,
            "bring it to [address].",
        ),
        (
            dana_dm("Back from İstanbul!\nDana says hi"),
            DANA_TG,
            "Back from İstanbul!\n{you} says hi",
        ),
    ];
    for (i, (event, space, scrubbed)) in cases.iter().enumerate() {
        inbound(&app, event).await;
        let sent = app.sidecar.wait_for(i + 1).await;
        assert_eq!(sent[i].body["space_id"], *space);
        assert_eq!(text(&sent[i].body), "Got it.");
        finished_turns(&pool, i + 1).await;
        let last = app.gemini.request_count() - 1;
        assert_eq!(last_user_text(&app, last), *scrubbed);
    }

    inbound(&app, &judge_dm("İ maya’Å")).await;
    let sent = app.sidecar.wait_for(4).await;
    app.sidecar.assert_no_more_than(4).await;
    assert_eq!(sent[3].body["space_id"], JUDGE_TG);
    assert_ne!(text(&sent[3].body), SNAG);
    assert_eq!(text(&sent[3].body), "Got it.");
    let turns = finished_turns(&pool, 4).await;
    assert_eq!(turns.len(), 4);
    let (said, reply, _) = &turns[3];
    assert_eq!(said, "İ maya’Å");
    assert_eq!(reply.as_deref(), Some("Got it."));
    assert_no_errors(&turns);
    let last = app.gemini.request_count() - 1;
    let scrubbed = last_user_text(&app, last);
    assert!(
        scrubbed.starts_with("İ {person_") && scrubbed.ends_with("}’Å"),
        "{scrubbed:?}"
    );
    assert_never_sent(&app, &["sam_k99", "Pittsboro", "Dana", "maya"]);
    app.gemini.assert_private();
}

/// MINOR 7: a member named "Chicken Little" ("Chicken" is a word of the menu and the tools'
/// declarations) refuses no request: the judge's DMs go through Gemini with no privacy refusal,
/// "chicken burrito no beans" reaches Gemini and the draft tool as written and is priced, and the
/// full name reaches Gemini only as a placeholder.
#[sqlx::test(migrations = "../migrations")]
async fn a_member_named_after_a_menu_word_refuses_nothing_and_drafting_still_prices(pool: PgPool) {
    seed_with_money(&pool).await;
    set_display_name(&pool, "usr_leo", "Chicken Little").await;
    let app = app(pool.clone(), true, &[]).await;
    // Gemini drafts an order from the user's words exactly as it got them; anything else gets a
    // line with no fact.
    gemini_with(&app, |said| {
        if said.contains("burrito") {
            call_reply("draft_order_from_text", json!({"text": said}))
        } else {
            text_reply("I can't see other people's orders.")
        }
    });

    inbound(&app, &judge_dm("did Chicken Little order yet?")).await;
    let sent = app.sidecar.wait_for(1).await;
    assert_eq!(text(&sent[0].body), "I can't see other people's orders.");
    assert!(app.gemini.request_count() > 0);
    let asked = last_user_text(&app, 0);
    assert!(
        asked.starts_with("did {person_") && asked.ends_with("} order yet?"),
        "{asked:?}"
    );
    finished_turns(&pool, 1).await;

    let before = app.gemini.request_count();
    inbound(&app, &judge_dm("chicken burrito no beans")).await;
    let sent = app.sidecar.wait_for(2).await;
    assert_eq!(text(&sent[1].body), draft_line("Fetzer Gym (HackNC)"));
    assert_eq!(last_user_text(&app, before), "chicken burrito no beans");
    let results = app.gemini.function_responses(before + 1);
    assert_eq!(results[0].0, "draft_order_from_text");
    assert_eq!(results[0].1["total"], "$13.16");
    let turns = finished_turns(&pool, 2).await;
    assert!(
        turns
            .iter()
            .all(|(_, _, error)| error.as_deref() != Some("privacy_guard")),
        "{turns:?}"
    );
    assert_no_errors(&turns);
    assert_never_sent(&app, &["Chicken Little"]);
    app.gemini.assert_private();
}
