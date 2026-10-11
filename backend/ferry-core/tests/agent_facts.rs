//! PR #22's review (orchestration/reports/agent-review.md) through `POST /internal/messages/inbound`,
//! with a stub sidecar and a stub Gemini (never the real one): facts come from code, not from
//! Gemini's words (MAJOR 1, BLOCKER 1b's "Here is the menu."), a reply cut short is never sent
//! (MINOR 11), the demo trip is announced again after a reset or a cancelled run (MAJOR 3), only
//! one group maps to the demo circle by title (MINOR 1), and `reply_to_bot` may be null (MINOR 10).
//! Every test that runs the agent ends with the privacy scan of what reached Gemini.
mod agent_support;
mod common;

use std::time::Duration;

use agent_support::{
    DANA_TG, DANA_USERNAME, GROUP_ID, GROUP_TITLE, JUDGE_TG, JUDGE_USERNAME, MAYA_TG,
    MAYA_USERNAME, Sent, TestApp, app, call_reply, dm, group_message, inbound, json_reply,
    map_demo_group, membership, seed_active_order, seed_demo, seed_ping, text_reply,
};
use ferry_core::agent::fallback::HELP;
use ferry_core::auth::telegram::WELCOME;
use ferry_core::demo::reset::reset;
use serde_json::{Value, json};
use sqlx::PgPool;

/// The ETA Gemini made up in the review: what the judge must never see.
const FAKE_ETA: &str = "{runner} is 1 min away (0.1 mi), seen 1 s ago.";
/// What Gemini said about a draft it had just confirmed, in BLOCKER 1b.
const MENU_LINE: &str = "Here is the menu.";
/// Words with no fact in them, as Gemini might stop mid-sentence.
const CUT_SHORT: &str = "Sure, here's what I can";
/// The judge's DM draft line for "chicken burrito no beans" on run_demo: $8.99, tax 7.5 % (67 c),
/// the $1.50 Ferry fee and the judge's default $2.00 tip.
const JUDGE_DM_DRAFT: &str = "Chicken Burrito (No beans) from Cosmic Cantina: $8.99 + tax $0.67 + Ferry fee $1.50 + tip $2.00 = $13.16, to Fetzer Gym (HackNC). Place the order?";
/// Maya's trip, as docs/DEMO.md has it.
const MAYAS_TRIP: &str = "heading to Cosmic Cantina at 6, anyone want anything?";

fn text(sent: &Value) -> &str {
    sent["text"].as_str().unwrap()
}

fn buttons(sent: &Value) -> Vec<Value> {
    sent["buttons"].as_array().cloned().unwrap_or_default()
}

/// The callback data of the button labelled `label`.
fn button_data(sent: &Value, label: &str) -> String {
    buttons(sent)
        .iter()
        .find(|b| b["label"] == label)
        .and_then(|b| b["callback_data"].as_str())
        .unwrap_or_else(|| panic!("no {label} button: {sent}"))
        .to_owned()
}

/// A generateContent answer whose text Gemini cut short at its token cap.
fn max_tokens_reply(text: &str) -> Value {
    json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": text}]},
            "finishReason": "MAX_TOKENS"
        }]
    })
}

/// The `say` of the tool result `body` ends with, when its last turn is a functionResponse.
fn tool_say(body: &Value) -> Option<String> {
    let last = body["contents"].as_array()?.last()?;
    let response = last["parts"][0].get("functionResponse")?;
    Some(
        response["response"]["say"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    )
}

/// Gemini for these tests: a trip classification says Cosmic Cantina; a turn calls `tool` with
/// `args` first, then answers `after(say)`, `say` being the tool's line.
fn gemini_calls(
    app: &TestApp,
    tool: &'static str,
    args: Value,
    after: impl Fn(&str) -> Value + Send + Sync + 'static,
) {
    app.gemini.respond_with(move |body| {
        if body["generationConfig"]["responseMimeType"] == "application/json" {
            return (
                200,
                json_reply(&json!({"is_trip": true, "restaurant_id": "rst_cosmic"})),
            );
        }
        match tool_say(body) {
            Some(say) => (200, after(&say)),
            None => (200, call_reply(tool, args.clone())),
        }
    });
}

/// The judge's funded order on run_demo, and Maya's ping 20 s ago on Franklin St just east of
/// Cosmic Cantina: get_eta has a real answer ("Maya is N min away (X mi), seen 20 s ago.").
async fn maya_walking(pool: &PgPool) {
    seed_active_order(pool, "ord_agent_judge", "usr_judge", "run_demo").await;
    seed_ping(pool, "run_demo", "usr_maya", 35.9125, -79.0520, 20).await;
}

/// The demo with money: the personas' starting balances (the judge has $50), the group mapped.
async fn seed_with_money(pool: &PgPool) {
    seed_demo(pool).await;
    map_demo_group(pool).await;
    ferry_core::payments::seed_demo_balances(pool)
        .await
        .unwrap();
}

/// A logged turn: (reply_text, error).
type TurnRow = (Option<String>, Option<String>);

/// The turns logged in `space_id`, oldest first, once `count` of them are finished (the log is
/// written after the sends).
async fn finished_turns(pool: &PgPool, space_id: &str, count: usize) -> Vec<TurnRow> {
    for _ in 0..300 {
        let rows: Vec<TurnRow> = sqlx::query_as(
            "SELECT reply_text, error FROM ferry.agent_turns
              WHERE space_id = $1 AND latency_ms IS NOT NULL
              ORDER BY id",
        )
        .bind(space_id)
        .fetch_all(pool)
        .await
        .unwrap();
        if rows.len() >= count {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("fewer than {count} finished turns in {space_id}");
}

async fn count(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

/// Maya's trip in the group, answered with a run's link; returns the run id.
async fn mayas_trip(app: &TestApp, already_sent: usize) -> String {
    let trip = group_message(MAYA_TG, Some(MAYA_USERNAME), "Maya", MAYAS_TRIP);
    assert!(inbound(app, &trip).await.is_empty());
    let sent = app.sidecar.wait_for(already_sent + 1).await;
    let reply = &sent[already_sent].body;
    assert_eq!(reply["space_id"], GROUP_ID);
    assert!(
        text(reply).starts_with("Ferry to Cosmic Cantina is open until "),
        "{reply}"
    );
    let url = buttons(reply)[0]["url"].as_str().unwrap().to_owned();
    url.strip_prefix("https://ferry.example/r/")
        .unwrap_or_else(|| panic!("not a run link: {url}"))
        .to_owned()
}

/// Waits up to `seconds` for the stub to hold `count` sends (the run announcer sweeps every 2 s).
async fn wait_long(app: &TestApp, count: usize, seconds: u64) -> Vec<Sent> {
    for _ in 0..seconds * 20 {
        let sent = app.sidecar.sent();
        if sent.len() >= count {
            return sent;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "the stub got {} sends, not {count}: {:#?}",
        app.sidecar.sent().len(),
        app.sidecar.sent()
    );
}

/// The circle `space_id` maps to.
async fn circle_of(pool: &PgPool, space_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT circle_id FROM ferry.message_spaces WHERE space_id = $1")
        .bind(space_id)
        .fetch_optional(pool)
        .await
        .unwrap()
        .flatten()
}

/// MAJOR 1 (agent-review.md): get_eta answered, then Gemini wrote its own ETA, "{runner} is 1 min
/// away (0.1 mi), seen 1 s ago.". The judge gets get_eta's `say` line from code, with Maya's name
/// filled in by code, and never Gemini's numbers.
#[sqlx::test(migrations = "../migrations")]
async fn geminis_made_up_eta_never_reaches_the_judge(pool: PgPool) {
    seed_demo(&pool).await;
    maya_walking(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    gemini_calls(&app, "get_eta", json!({}), |_| text_reply(FAKE_ETA));

    let ask = dm(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "how long until my food gets here?",
    );
    assert!(inbound(&app, &ask).await.is_empty());
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    let reply = &sent[0].body;
    assert_eq!(reply["space_id"], JUDGE_TG);
    let line = text(reply);

    assert_eq!(app.gemini.request_count(), 2);
    let results = app.gemini.function_responses(1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "get_eta");
    let say = results[0].1["say"].as_str().unwrap();
    assert!(say.starts_with("{runner} is "), "{say}");
    // A whole number of minutes, as JSON's float (16.0 shows as "16").
    let minutes = results[0].1["eta_minutes"]
        .as_f64()
        .unwrap_or_else(|| panic!("no eta_minutes: {}", results[0].1));
    let distance = results[0].1["distance"].as_str().unwrap();
    // Code's line, word for word, with the runner's name from code.
    assert_eq!(line, say.replace("{runner}", "Maya"));
    assert!(
        line.starts_with(&format!("Maya is {minutes} min away ({distance}), seen ")),
        "{line}"
    );
    assert!(
        !line.contains("1 min away (0.1 mi)") && !line.contains("seen 1 s ago"),
        "Gemini's numbers reached the judge: {line}"
    );
    // Code's line won outright: no fallback.
    let turns = finished_turns(&pool, JUDGE_TG, 1).await;
    assert_eq!(turns, [(Some(line.to_owned()), None)]);
    app.gemini.assert_private();
}

/// MAJOR 1: with no tool run, Gemini's "Your total is $1.00." states a fact no tool gave. The
/// judge's "hi" gets the fallback's answer instead (for "hi", the help line), and the turn
/// records why: "unsourced_facts".
#[sqlx::test(migrations = "../migrations")]
async fn a_fact_gemini_states_without_a_tool_is_refused(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    app.gemini.script(vec![text_reply("Your total is $1.00.")]);

    inbound(&app, &dm(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", "hi")).await;
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(sent[0].body["space_id"], JUDGE_TG);
    assert_eq!(text(&sent[0].body), HELP);
    assert_eq!(app.gemini.request_count(), 1);
    let turns = finished_turns(&pool, JUDGE_TG, 1).await;
    assert_eq!(
        turns,
        [(Some(HELP.to_owned()), Some("unsourced_facts".to_owned()))]
    );
    app.gemini.assert_private();
}

/// MAJOR 1 with BLOCKER 1b's "Here is the menu.": the judge drafts by DM, then asks "hmm how much
/// is that again? not sure I want it"; Gemini calls confirm_order and says "Here is the menu.".
/// The judge gets the same draft line again with its Place order and Cancel buttons, and nothing
/// is placed or held.
#[sqlx::test(migrations = "../migrations")]
async fn confirm_order_with_the_wrong_words_shows_the_draft_and_places_nothing(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    let holds_before = count(
        &pool,
        "SELECT count(*) FROM ferry.ledger_entries WHERE kind = 'order_hold'",
    )
    .await;

    gemini_calls(
        &app,
        "draft_order_from_text",
        json!({"text": "chicken burrito no beans"}),
        |_| text_reply(MENU_LINE),
    );
    let order = dm(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "chicken burrito no beans",
    );
    inbound(&app, &order).await;
    let sent = app.sidecar.wait_for(1).await;
    let draft = &sent[0].body;
    assert_eq!(draft["space_id"], JUDGE_TG);
    assert_eq!(text(draft), JUDGE_DM_DRAFT);
    let place = button_data(draft, "Place order");
    let cancel = button_data(draft, "Cancel");
    assert_eq!(app.gemini.request_count(), 2);

    gemini_calls(&app, "confirm_order", json!({}), |_| text_reply(MENU_LINE));
    let unsure = dm(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "hmm how much is that again? not sure I want it",
    );
    inbound(&app, &unsure).await;
    let sent = app.sidecar.wait_for(2).await;
    app.sidecar.assert_no_more_than(2).await;
    let again = &sent[1].body;
    assert_eq!(again["space_id"], JUDGE_TG);
    assert_eq!(
        text(again),
        text(draft),
        "the draft line again, not Gemini's"
    );
    assert_eq!(button_data(again, "Place order"), place);
    assert_eq!(button_data(again, "Cancel"), cancel);

    // The message went through Gemini's tool loop (not an edit of the draft): confirm_order ran
    // and placed nothing.
    assert_eq!(app.gemini.request_count(), 4);
    let results = app.gemini.function_responses(3);
    let confirm = results
        .iter()
        .find(|(name, _)| name == "confirm_order")
        .unwrap_or_else(|| panic!("no confirm_order result: {results:?}"));
    assert_eq!(confirm.1["placed"], false);

    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM ferry.orders WHERE NOT is_demo_history"
        )
        .await,
        0,
        "words never place an order"
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM ferry.ledger_entries WHERE kind = 'order_hold'"
        )
        .await,
        holds_before,
        "nothing is held"
    );
    let drafts: Vec<String> = sqlx::query_scalar("SELECT status FROM ferry.order_drafts")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(drafts, ["pending"]);
    let turns = finished_turns(&pool, JUDGE_TG, 2).await;
    assert_eq!(
        turns,
        [
            (Some(JUDGE_DM_DRAFT.to_owned()), None),
            (Some(JUDGE_DM_DRAFT.to_owned()), None)
        ]
    );
    app.gemini.assert_private();
}

/// MINOR 11: Gemini's text cut short by its token cap (finishReason MAX_TOKENS), with no tool, is
/// never sent: the fallback answers ("hi": the help line) and the turn records "empty_answer".
/// The same words finished (STOP) are sent as they are, so the cut is what refused them.
#[sqlx::test(migrations = "../migrations")]
async fn a_reply_cut_short_without_a_tool_is_not_sent(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    app.gemini
        .script(vec![max_tokens_reply(CUT_SHORT), text_reply(CUT_SHORT)]);

    inbound(&app, &dm(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", "hi")).await;
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(text(&sent[0].body), HELP);
    finished_turns(&pool, JUDGE_TG, 1).await;

    inbound(&app, &dm(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", "hello")).await;
    let sent = app.sidecar.wait_for(2).await;
    app.sidecar.assert_no_more_than(2).await;
    assert_eq!(text(&sent[1].body), CUT_SHORT);

    assert_eq!(app.gemini.request_count(), 2);
    let turns = finished_turns(&pool, JUDGE_TG, 2).await;
    assert_eq!(
        turns,
        [
            (Some(HELP.to_owned()), Some("empty_answer".to_owned())),
            (Some(CUT_SHORT.to_owned()), None)
        ]
    );
    app.gemini.assert_private();
}

/// MINOR 11 after a tool: get_eta answered, then Gemini's text was cut short (MAX_TOKENS). It
/// carries the tool's line word for word and states no fact of its own, so finished it would be
/// sent; cut short, the judge gets the tool's line alone.
#[sqlx::test(migrations = "../migrations")]
async fn a_reply_cut_short_after_a_tool_gives_the_tools_line(pool: PgPool) {
    seed_demo(&pool).await;
    maya_walking(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    gemini_calls(&app, "get_eta", json!({}), |say| {
        max_tokens_reply(&format!("Good news! {say} Enjoy your"))
    });

    let ask = dm(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "how long until my food gets here?",
    );
    inbound(&app, &ask).await;
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    let line = text(&sent[0].body);
    let results = app.gemini.function_responses(1);
    assert_eq!(results[0].0, "get_eta");
    let say = results[0].1["say"].as_str().unwrap();
    assert_eq!(line, say.replace("{runner}", "Maya"));
    assert!(
        !line.contains("Good news") && !line.contains("Enjoy"),
        "{line}"
    );
    let turns = finished_turns(&pool, JUDGE_TG, 1).await;
    assert_eq!(turns, [(Some(line.to_owned()), None)]);
    app.gemini.assert_private();
}

/// MAJOR 3: a rehearsal's trip, then `ferry-core reset`, then the same trip in the live demo gets
/// its reply with run_demo's link again, not two hours of silence. The reset deletes the trip
/// intents; its re-seed keeps the personas' Telegram links and the group's mapping.
#[sqlx::test(migrations = "../migrations")]
async fn after_a_demo_reset_the_same_trip_is_announced_again(pool: PgPool) {
    seed_demo(&pool).await;
    map_demo_group(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    gemini_calls(&app, "get_profile", json!({}), text_reply);

    assert_eq!(mayas_trip(&app, 0).await, "run_demo");
    app.sidecar.assert_no_more_than(1).await;

    let report = reset(&pool, &common::config(&[("DEMO_MODE", "true")]))
        .await
        .unwrap();
    assert_eq!(report.deleted("trip_intents"), Some(1), "{report}");
    // Nothing to re-link: the re-seed leaves the Telegram ids, and the group still maps.
    let linked: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT id, telegram_user_id FROM ferry.users
          WHERE id IN ('usr_judge', 'usr_maya', 'usr_dana') ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        linked,
        [
            ("usr_dana".to_owned(), Some(DANA_TG.to_owned())),
            ("usr_judge".to_owned(), Some(JUDGE_TG.to_owned())),
            ("usr_maya".to_owned(), Some(MAYA_TG.to_owned())),
        ]
    );
    assert_eq!(
        circle_of(&pool, GROUP_ID).await.as_deref(),
        Some("cir_hinton4")
    );

    assert_eq!(mayas_trip(&app, 1).await, "run_demo");
    app.sidecar.assert_no_more_than(2).await;
    let intents: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT status, run_id FROM ferry.trip_intents")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        intents,
        [("opened".to_owned(), Some("run_demo".to_owned()))]
    );
    assert_eq!(app.gemini.request_count(), 2, "one classification each");
    app.gemini.assert_private();
}

/// MAJOR 3: ask once still holds while the trip is live (the repeat is silent), but an intent
/// whose run was cancelled no longer counts: after the cancellation's announcement, the same trip
/// is answered again, with a new run's link.
#[sqlx::test(migrations = "../migrations")]
async fn a_live_trip_is_answered_once_and_a_cancelled_runs_trip_again(pool: PgPool) {
    seed_demo(&pool).await;
    map_demo_group(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    gemini_calls(&app, "get_profile", json!({}), text_reply);

    assert_eq!(mayas_trip(&app, 0).await, "run_demo");
    let again = group_message(MAYA_TG, Some(MAYA_USERNAME), "Maya", MAYAS_TRIP);
    inbound(&app, &again).await;
    app.sidecar.assert_no_more_than(1).await;

    sqlx::query("UPDATE ferry.runs SET status = 'cancelled' WHERE id = 'run_demo'")
        .execute(&pool)
        .await
        .unwrap();
    // The run announcer's sweep tells the group first.
    let sent = wait_long(&app, 2, 8).await;
    assert_eq!(sent[1].body["space_id"], GROUP_ID);
    assert_eq!(
        text(&sent[1].body),
        "The Ferry to Cosmic Cantina was cancelled."
    );

    let run_id = mayas_trip(&app, 2).await;
    assert_ne!(run_id, "run_demo", "a new run opens");
    app.sidecar.assert_no_more_than(3).await;
    let intents: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT status, run_id FROM ferry.trip_intents ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        intents,
        [
            ("opened".to_owned(), Some("run_demo".to_owned())),
            ("opened".to_owned(), Some(run_id)),
        ]
    );
    app.gemini.assert_private();
}

/// MINOR 1: in DEMO_MODE the demo group ("Hinton James 4th floor") maps to cir_hinton4, and a
/// later group given the same title maps to a new circle of its own, whether it maps on a
/// membership event or on a linked user's (Dana's) first message there. cir_hinton4 stays the
/// demo group's alone.
#[sqlx::test(migrations = "../migrations")]
async fn only_the_demo_group_maps_to_the_demo_circle_by_title(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    let by_membership = "-1009990001";
    let by_message = "-1009990002";

    let added = membership(
        GROUP_ID,
        true,
        MAYA_TG,
        Some(GROUP_TITLE),
        true,
        Some(false),
        None,
    );
    inbound(&app, &added).await;
    assert_eq!(
        circle_of(&pool, GROUP_ID).await.as_deref(),
        Some("cir_hinton4")
    );

    // Dana adds the bot to her own group with the same title.
    let copied = membership(
        by_membership,
        true,
        DANA_TG,
        Some(GROUP_TITLE),
        true,
        Some(false),
        None,
    );
    inbound(&app, &copied).await;
    let first = circle_of(&pool, by_membership)
        .await
        .expect("the group has a circle");
    assert_ne!(first, "cir_hinton4");

    // Another such group maps on Dana's first message there.
    let mut chatter = group_message(DANA_TG, Some(DANA_USERNAME), "Dana", "lol same");
    chatter["space_id"] = json!(by_message);
    assert_eq!(chatter["chat_title"], GROUP_TITLE);
    inbound(&app, &chatter).await;
    let second = circle_of(&pool, by_message)
        .await
        .expect("the group has a circle");
    assert_ne!(second, "cir_hinton4");
    assert_ne!(second, first);

    for circle in [&first, &second] {
        let (name, emoji): (String, String) =
            sqlx::query_as("SELECT name, emoji FROM ferry.circles WHERE id = $1")
                .bind(circle)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((name.as_str(), emoji.as_str()), (GROUP_TITLE, "👥"));
    }
    let demo_spaces: Vec<String> = sqlx::query_scalar(
        "SELECT space_id FROM ferry.message_spaces WHERE circle_id = 'cir_hinton4'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(demo_spaces, [GROUP_ID]);
    // Two hellos (the two membership events); the lazy mapping and "lol same" say nothing.
    app.sidecar.wait_for(2).await;
    app.sidecar.assert_no_more_than(2).await;
}

/// MINOR 10: the sidecar does not send `reply_to_bot` yet, so it may be null or absent; either
/// way the event is accepted (200) and an unaddressed group message stays silent. `true` makes
/// it the bot's: one answer. Sign-in with a null field is accepted too.
#[sqlx::test(migrations = "../migrations")]
async fn reply_to_bot_may_be_null_or_absent_and_true_addresses_the_bot(pool: PgPool) {
    seed_demo(&pool).await;
    map_demo_group(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    let lol = || group_message(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", "lol same");

    let mut null = lol();
    null["reply_to_bot"] = Value::Null;
    assert!(inbound(&app, &null).await.is_empty());
    let absent = lol();
    assert!(absent.get("reply_to_bot").is_none());
    assert!(inbound(&app, &absent).await.is_empty());
    app.sidecar.assert_no_more_than(0).await;
    assert_eq!(
        count(&pool, "SELECT count(*) FROM ferry.agent_turns").await,
        0
    );

    let mut replied = lol();
    replied["reply_to_bot"] = json!(true);
    assert!(inbound(&app, &replied).await.is_empty());
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(sent[0].body["space_id"], GROUP_ID);
    assert_eq!(text(&sent[0].body), HELP, "Gemini is off: the fallback");
    assert_eq!(
        count(&pool, "SELECT count(*) FROM ferry.agent_turns").await,
        1
    );

    let mut start = dm(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", "/start");
    start["reply_to_bot"] = Value::Null;
    inbound(&app, &start).await;
    let sent = app.sidecar.wait_for(2).await;
    assert_eq!(sent[1].target().as_deref(), Some(JUDGE_TG));
    assert_eq!(text(&sent[1].body), WELCOME);
}
