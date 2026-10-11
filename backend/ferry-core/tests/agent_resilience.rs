//! The agent when Gemini is slow, loops, misbehaves, refuses or is off, through
//! `POST /internal/messages/inbound` with a stub sidecar and a stub Gemini (never the real one):
//! the sidecar is answered at once, the tool loop stops after four rounds, bad answers fall back
//! to the same tools, the key and model are used as configured, and sign-in keeps its own path.
mod agent_support;
mod common;

use std::time::{Duration, Instant};

use agent_support::{
    GEMINI_KEY, GEMINI_MODEL, JUDGE_TG, JUDGE_USERNAME, STRANGER_TG, TestApp, app, call_reply, dm,
    inbound, seed_demo, text_reply,
};
use ferry_core::agent::dispatch::UNKNOWN_SENDER;
use ferry_core::auth::telegram::WELCOME;
use serde_json::{Value, json};
use sqlx::PgPool;

fn text(sent: &Value) -> &str {
    sent["text"].as_str().unwrap()
}

/// The judge's DM saying `words`.
fn judge_says(words: &str) -> Value {
    dm(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", words)
}

/// A finished turn's (reply_text, tool_calls, model, error).
type TurnRow = (Option<String>, Value, Option<String>, Option<String>);

/// The newest turn logged for `space_id`, once it is finished (the log is written after the
/// sends).
async fn finished_turn(pool: &PgPool, space_id: &str) -> TurnRow {
    for _ in 0..500 {
        let row: Option<TurnRow> = sqlx::query_as(
            "SELECT reply_text, tool_calls, model, error FROM ferry.agent_turns
              WHERE space_id = $1 AND latency_ms IS NOT NULL
              ORDER BY id DESC LIMIT 1",
        )
        .bind(space_id)
        .fetch_optional(pool)
        .await
        .unwrap();
        if let Some(row) = row {
            return row;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no finished turn for {space_id}");
}

/// The names in a turn's `tool_calls`, in order.
fn call_names(calls: &Value) -> Vec<&str> {
    calls
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect()
}

/// Whether the last entry of a request's `contents` holds a function response (Gemini is being
/// handed a tool's result).
fn after_a_tool(body: &Value) -> bool {
    body["contents"]
        .as_array()
        .and_then(|c| c.last())
        .is_some_and(|last| last["parts"][0].get("functionResponse").is_some())
}

/// The Chapel Hill hour now, from the database (the catalog computes `is_open` from it).
async fn local_hour(pool: &PgPool) -> i32 {
    sqlx::query_scalar("SELECT extract(hour FROM now() AT TIME ZONE 'America/New_York')::int")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// list_restaurants' answer to "what's open?". The fixtures' hours leave nothing open from 04:00
/// to 07:00 in Chapel Hill; then the line says so instead.
async fn assert_open_line(pool: &PgPool, line: &str) {
    if (4..7).contains(&local_hour(pool).await) {
        assert!(
            line.starts_with("Open now:") || line == "Nothing that matches is open right now.",
            "{line}"
        );
    } else {
        assert!(line.starts_with("Open now:"), "{line}");
    }
}

/// Waits up to `within` for `count` sends (the stub's own `wait_for` gives up after 5 s).
async fn sends_within(app: &TestApp, count: usize, within: Duration) -> Vec<Value> {
    let started = Instant::now();
    while started.elapsed() < within {
        let sent = app.sidecar.sent();
        if sent.len() >= count {
            return sent.into_iter().map(|s| s.body).collect();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "{} sends after {within:?}, not {count}: {:#?}",
        app.sidecar.sent().len(),
        app.sidecar.sent()
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn a_slow_gemini_still_answers_after_the_inbound_returns(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    app.gemini.set_delay(Duration::from_millis(3500));
    app.gemini.script(vec![text_reply("Hi from Ferry.")]);

    let started = Instant::now();
    let replies = inbound(&app, &judge_says("hi")).await;
    let answered_in = started.elapsed();
    assert!(replies.is_empty());
    assert!(
        answered_in < Duration::from_millis(1000),
        "the sidecar waited {answered_in:?}"
    );
    assert!(app.sidecar.sent().is_empty(), "Gemini has not answered yet");

    let sent = sends_within(&app, 1, Duration::from_secs(15)).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(sent[0]["space_id"], JUDGE_TG);
    assert_eq!(text(&sent[0]), "Hi from Ferry.");
    assert_eq!(app.gemini.request_count(), 1);
    let (reply, _, _, error) = finished_turn(&pool, JUDGE_TG).await;
    assert_eq!(reply.as_deref(), Some("Hi from Ferry."));
    assert_eq!(error, None);
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn at_most_four_tool_rounds_then_gemini_must_answer(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    // A Gemini that would call get_wallet forever, unless tools are off.
    app.gemini.respond_with(|body| {
        if body["toolConfig"]["functionCallingConfig"]["mode"] == "NONE" {
            (200, text_reply("Here is your balance."))
        } else {
            (200, call_reply("get_wallet", json!({})))
        }
    });

    inbound(&app, &judge_says("what's my balance?")).await;
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    // Gemini's words don't carry the tool's line, so the line itself goes out (once).
    assert_eq!(text(&sent[0].body), "Your Ferry balance is $0.00.");
    let requests = app.gemini.requests();
    assert_eq!(requests.len(), 5);
    let modes: Vec<&str> = requests
        .iter()
        .map(|r| {
            r.body["toolConfig"]["functionCallingConfig"]["mode"]
                .as_str()
                .unwrap()
        })
        .collect();
    assert_eq!(modes, ["AUTO", "AUTO", "AUTO", "AUTO", "NONE"]);
    // The last request carries all four results.
    let results = app.gemini.function_responses(4);
    assert_eq!(results.len(), 4);
    assert!(results.iter().all(|(name, _)| name == "get_wallet"));

    let (reply, calls, model, error) = finished_turn(&pool, JUDGE_TG).await;
    assert_eq!(reply.as_deref(), Some("Your Ferry balance is $0.00."));
    assert_eq!(model.as_deref(), Some(GEMINI_MODEL));
    assert_eq!(error, None);
    let names = call_names(&calls);
    assert_eq!(names.iter().filter(|n| **n == "gemini").count(), 5);
    assert_eq!(names.iter().filter(|n| **n == "get_wallet").count(), 4);
    assert_eq!(
        names,
        [
            "gemini",
            "get_wallet",
            "gemini",
            "get_wallet",
            "gemini",
            "get_wallet",
            "gemini",
            "get_wallet",
            "gemini"
        ]
    );
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn an_unknown_tool_name_gets_an_error_result(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    app.gemini.script(vec![
        call_reply("book_table", json!({})),
        text_reply("I can't do that yet."),
    ]);

    inbound(&app, &judge_says("can you book us a table for six?")).await;
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(text(&sent[0].body), "I can't do that yet.");
    assert_eq!(app.gemini.request_count(), 2);
    let results = app.gemini.function_responses(1);
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].0, "book_table");
    assert!(results[0].1["error"].is_string(), "{}", results[0].1);
    let (reply, calls, _, error) = finished_turn(&pool, JUDGE_TG).await;
    assert_eq!(reply.as_deref(), Some("I can't do that yet."));
    assert_eq!(error, None);
    assert_eq!(call_names(&calls), ["gemini", "gemini"], "no tool ran");
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn an_empty_answer_after_a_tool_falls_back_to_the_tools_line(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    app.gemini.respond_with(|body| {
        if after_a_tool(body) {
            (
                200,
                json!({"candidates": [{"content": {"role": "model", "parts": []}, "finishReason": "STOP"}]}),
            )
        } else {
            (200, call_reply("get_wallet", json!({})))
        }
    });

    inbound(&app, &judge_says("what's my balance?")).await;
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    let line = text(&sent[0].body);
    assert!(line.starts_with("Your Ferry balance is $"), "{line}");
    // An empty 200 is not retried.
    assert_eq!(app.gemini.request_count(), 2);
    let (reply, calls, _, error) = finished_turn(&pool, JUDGE_TG).await;
    assert_eq!(reply.as_deref(), Some(line));
    assert_eq!(error.as_deref(), Some("empty"));
    // The wallet already answered, so its line is the reply; the tool does not run again.
    assert_eq!(call_names(&calls), ["gemini", "get_wallet", "gemini"]);
    assert_eq!(calls[2]["ok"], false);
    assert_eq!(calls[2]["error"], "empty");
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn a_400_from_gemini_is_not_retried_and_falls_back(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    app.gemini.set_down(400);

    inbound(&app, &judge_says("what's open?")).await;
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_open_line(&pool, text(&sent[0].body)).await;
    assert_eq!(app.gemini.request_count(), 1, "a 400 gets no retry");
    let (_, calls, _, error) = finished_turn(&pool, JUDGE_TG).await;
    assert_eq!(error.as_deref(), Some("status"));
    assert_eq!(call_names(&calls), ["gemini", "list_restaurants"]);
    assert_eq!(calls[0]["ok"], false);
    assert_eq!(calls[0]["error"], "status");
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn a_400_is_logged_as_one_attempt(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    app.gemini.set_down(400);

    inbound(&app, &judge_says("what's open?")).await;
    app.sidecar.wait_for(1).await;
    assert_eq!(app.gemini.request_count(), 1);
    let (_, calls, _, _) = finished_turn(&pool, JUDGE_TG).await;
    assert_eq!(calls[0]["args"]["attempts"], 1, "{calls}");
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn without_gemini_the_fallback_answers_impact(pool: PgPool) {
    seed_demo(&pool).await;
    // The demo history gives the judge walking deliveries.
    ferry_core::impact::seed_history(&pool).await.unwrap();
    let app = app(pool.clone(), false, &[]).await;

    inbound(&app, &judge_says("how much co2 have we saved")).await;
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    let line = text(&sent[0].body);
    assert!(
        line.contains("CO2") && line.contains("EPA estimate"),
        "{line}"
    );
    assert_eq!(app.gemini.request_count(), 0);
    let (_, calls, model, error) = finished_turn(&pool, JUDGE_TG).await;
    assert_eq!(model, None);
    assert_eq!(error.as_deref(), Some("gemini_unconfigured"));
    assert_eq!(call_names(&calls), ["explain_impact"]);
}

#[sqlx::test(migrations = "../migrations")]
async fn the_key_and_model_are_used_as_configured(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    app.gemini.script(vec![text_reply("Hello.")]);

    inbound(&app, &judge_says("hi there")).await;
    app.sidecar.wait_for(1).await;
    let requests = app.gemini.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.api_key.as_deref(), Some(GEMINI_KEY));
    assert!(
        request
            .path
            .ends_with(&format!("{GEMINI_MODEL}:generateContent")),
        "{}",
        request.path
    );
    assert!(
        !request.path.contains(GEMINI_KEY),
        "the key never goes in the URL"
    );
    let declarations = request.body["tools"][0]["functionDeclarations"]
        .as_array()
        .unwrap();
    let names: Vec<&str> = declarations
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "get_profile",
            "list_restaurants",
            "get_menu",
            "get_wallet",
            "explain_impact",
            "get_eta",
            "start_meet_again",
            "respond_invite",
            "list_open_runs",
            "open_run",
            "draft_order_from_text",
            "confirm_order",
            "cancel_order",
            "get_order_status"
        ]
    );
    let system = request.body["systemInstruction"]["parts"][0]["text"]
        .as_str()
        .unwrap();
    assert!(system.contains("{you}"), "{system}");
    assert!(!system.contains("Jordan"), "{system}");
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn sign_in_keeps_its_own_path(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;

    let start = dm(STRANGER_TG, Some("stranger"), "Sam", "/start");
    assert!(
        inbound(&app, &start).await.is_empty(),
        "a line with a button goes through /send"
    );
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(sent[0].target().as_deref(), Some(STRANGER_TG));
    assert_eq!(text(&sent[0].body), WELCOME);
    assert_eq!(sent[0].body["buttons"][0]["label"], "Open Ferry");
    let turns: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.agent_turns")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(turns, 0, "sign-in is not an agent turn");
    let told: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.unknown_senders")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(told, 0, "the welcome is not the unknown-sender line");

    // The stranger's first message to the agent still gets the line, once.
    inbound(&app, &dm(STRANGER_TG, Some("stranger"), "Sam", "hi")).await;
    let sent = app.sidecar.wait_for(2).await;
    assert_eq!(text(&sent[1].body), UNKNOWN_SENDER);
    inbound(&app, &dm(STRANGER_TG, Some("stranger"), "Sam", "hi?")).await;
    app.sidecar.assert_no_more_than(2).await;
    assert_eq!(app.gemini.request_count(), 0);
    app.gemini.assert_private();
}

/// PR #22's fix round: a DM that looks like a trip ("going to Target at 5") but names no
/// restaurant. With Gemini down, the trip classifier fails (one call, one retry) and the turn's
/// answer comes from the fallback without calling Gemini again: a Gemini that never answers would
/// otherwise be waited on twice (2 × 24.3 s), past the job's 45 s.
#[sqlx::test(migrations = "../migrations")]
async fn once_gemini_failed_in_a_turn_it_is_not_called_again(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    app.gemini.set_down(503);

    inbound(
        &app,
        &judge_says("going to Target at 5, anyone want anything?"),
    )
    .await;
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(sent[0].body["space_id"], JUDGE_TG);
    assert_eq!(text(&sent[0].body), ferry_core::agent::fallback::HELP);
    assert_eq!(
        app.gemini.request_count(),
        2,
        "the classifier's call and its retry, then no more"
    );
    let requests = app.gemini.requests();
    assert!(
        requests
            .iter()
            .all(|r| r.body["generationConfig"]["responseMimeType"] == "application/json"),
        "only the classifier called Gemini"
    );
    let (reply, _, _, error) = finished_turn(&pool, JUDGE_TG).await;
    assert_eq!(reply.as_deref(), Some(ferry_core::agent::fallback::HELP));
    assert_eq!(error.as_deref(), Some("status"));
    app.gemini.assert_private();
}

/// Round 2's timing note: the trip classifier and the answer share the turn's 25 s for Gemini. A
/// slow Gemini (11 s a call, inside the 12 s attempt timeout) answers the classifier at 11 s and
/// the answer's first round at 22 s; the second round gets only what is left, and the turn ends
/// near 25 s with the tool's line. Without the shared budget the answer would get a fresh 25 s
/// (36 s in all), and a classifier answered on its retry (~23 s) plus an answer timing out twice
/// (24.3 s) would pass the job's 45 s.
#[sqlx::test(migrations = "../migrations")]
async fn the_classifier_and_the_answer_share_the_turns_gemini_budget(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    app.gemini.set_delay(Duration::from_secs(11));
    app.gemini.script(vec![
        agent_support::json_reply(&json!({"is_trip": false, "restaurant_id": "none"})),
        call_reply("list_restaurants", json!({})),
        text_reply("too late"),
    ]);

    let started = Instant::now();
    inbound(
        &app,
        &judge_says("going to Target at 5, anyone want anything?"),
    )
    .await;
    let sent = sends_within(&app, 1, Duration::from_secs(40)).await;
    let took = started.elapsed();
    assert!(
        took < Duration::from_secs(27),
        "every Gemini call of the turn within its 25 s: {took:?}"
    );
    assert_eq!(sent[0]["space_id"], JUDGE_TG);
    assert_open_line(&pool, text(&sent[0])).await;
    assert_eq!(
        app.gemini.request_count(),
        3,
        "the classifier, round 1, and round 2 cut short"
    );
    let (_, calls, _, error) = finished_turn(&pool, JUDGE_TG).await;
    assert_eq!(error.as_deref(), Some("turn_budget"));
    assert_eq!(
        call_names(&calls),
        ["gemini", "gemini", "list_restaurants"],
        "the cut round is not logged as a call"
    );
}
