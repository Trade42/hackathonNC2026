//! Phase B through `POST /internal/messages/inbound`, a stub sidecar and a stub Gemini: Maya's trip
//! opens (or reuses) the run with its link, the judge's "chicken burrito no beans" in the group
//! becomes a priced draft and then a placed order, edits and refusals, the run's state changes
//! announced once in the group, and ordering in a DM through Gemini's tools and the fallback.
mod agent_support;
mod common;

use std::time::Duration;

use agent_support::{
    DANA_TG, DANA_USERNAME, GROUP_ID, JUDGE_TG, JUDGE_USERNAME, MAYA_TG, MAYA_USERNAME,
    STRANGER_TG, TestApp, app, call_reply, dm, group_message, inbound, json_reply, map_demo_group,
    press, seed_demo, text_reply,
};
use ferry_core::agent::dispatch::UNKNOWN_SENDER;
use ferry_core::runs::{self, MarkPurchasedRequest};
use serde_json::{Value, json};
use sqlx::PgPool;

/// The judge's draft line for "chicken burrito no beans" on run_demo: $8.99, tax 7.5 % (67 c),
/// the $1.50 Ferry fee and the judge's default $2.00 tip.
const JUDGE_DRAFT: &str = "Jordan: Chicken Burrito (No beans) from Cosmic Cantina: $8.99 + tax $0.67 + Ferry fee $1.50 + tip $2.00 = $13.16, to Fetzer Gym (HackNC). Place the order?";

/// Gemini for these tests: a trip classification says Cosmic Cantina; a turn calls `tool` with
/// `args` first, then repeats the tool's `say` line.
fn scripted_gemini(app: &TestApp, tool: &'static str, args: Value) {
    app.gemini.respond_with(move |body| {
        if body["generationConfig"]["responseMimeType"] == "application/json" {
            return (
                200,
                json_reply(&json!({"is_trip": true, "restaurant_id": "rst_cosmic"})),
            );
        }
        let contents = body["contents"].as_array().unwrap();
        let last = &contents[contents.len() - 1];
        match last["parts"][0].get("functionResponse") {
            Some(response) => (
                200,
                text_reply(response["response"]["say"].as_str().unwrap_or("done")),
            ),
            None => (200, call_reply(tool, args.clone())),
        }
    });
}

fn text(sent: &Value) -> &str {
    sent["text"].as_str().unwrap()
}

fn buttons(sent: &Value) -> Vec<Value> {
    sent["buttons"].as_array().cloned().unwrap_or_default()
}

/// The demo with money: the personas' starting balances (the judge has $50).
async fn seed_with_money(pool: &PgPool) {
    seed_demo(pool).await;
    map_demo_group(pool).await;
    ferry_core::payments::seed_demo_balances(pool)
        .await
        .unwrap();
}

/// Maya's trip in the group, answered with the run's link; returns the run id.
async fn mayas_trip(app: &TestApp, already_sent: usize) -> String {
    let trip = group_message(
        MAYA_TG,
        Some(MAYA_USERNAME),
        "Maya",
        "heading to Cosmic Cantina at 6, anyone want anything?",
    );
    assert!(inbound(app, &trip).await.is_empty());
    let sent = app.sidecar.wait_for(already_sent + 1).await;
    let reply = &sent[already_sent].body;
    assert_eq!(reply["space_id"], GROUP_ID);
    let url = buttons(reply)[0]["url"].as_str().unwrap().to_owned();
    url.strip_prefix("https://ferry.example/r/")
        .unwrap_or_else(|| panic!("not a run link: {url}"))
        .to_owned()
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

/// Waits up to `seconds` for the stub to hold `count` sends.
async fn wait_long(app: &TestApp, count: usize, seconds: u64) -> Vec<agent_support::Sent> {
    for _ in 0..seconds * 20 {
        let sent = app.sidecar.sent();
        if sent.len() >= count {
            return sent;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "the stub got {} sends, not {count}",
        app.sidecar.sent().len()
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn mayas_trip_announces_the_demo_run_with_its_link(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    scripted_gemini(&app, "get_profile", json!({}));

    let run_id = mayas_trip(&app, 0).await;
    app.sidecar.assert_no_more_than(1).await;
    // Maya already runs the seeded run_demo at Cosmic Cantina in this circle: that one is it.
    assert_eq!(run_id, "run_demo");
    let sent = app.sidecar.sent();
    let closes: String = sqlx::query_scalar(
        "SELECT to_char(window_closes_at AT TIME ZONE 'America/New_York', 'FMHH12:MI')
           FROM ferry.runs WHERE id = 'run_demo'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        text(&sent[0].body),
        format!(
            "Ferry to Cosmic Cantina is open until {closes}. Reply with your order or tap the button."
        )
    );
    let (status, run): (String, Option<String>) =
        sqlx::query_as("SELECT status, run_id FROM ferry.trip_intents")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (status.as_str(), run.as_deref()),
        ("opened", Some("run_demo"))
    );
    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.runs WHERE NOT is_demo_history")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 1, "no second run");
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn a_sure_trip_without_an_open_run_opens_one_from_the_message(pool: PgPool) {
    seed_with_money(&pool).await;
    sqlx::query("UPDATE ferry.runs SET status = 'cancelled' WHERE id = 'run_demo'")
        .execute(&pool)
        .await
        .unwrap();
    let app = app(pool.clone(), true, &[]).await;
    scripted_gemini(&app, "get_profile", json!({}));

    let run_id = mayas_trip(&app, 0).await;
    assert_ne!(run_id, "run_demo");
    let (runner, circle, restaurant, status, via, window): (
        String,
        String,
        String,
        String,
        String,
        f64,
    ) = sqlx::query_as(
        "SELECT runner_id, circle_id, restaurant_id, status, created_via,
                    extract(epoch FROM window_closes_at - leaving_at)::float8
               FROM ferry.runs WHERE id = $1",
    )
    .bind(&run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (runner.as_str(), circle.as_str(), restaurant.as_str()),
        ("usr_maya", "cir_hinton4", "rst_cosmic")
    );
    assert_eq!((status.as_str(), via.as_str()), ("open", "message"));
    assert_eq!(window, 600.0, "orders close 10 minutes after leaving");
    let reply = &app.sidecar.sent()[0].body;
    assert!(
        text(reply).starts_with("Ferry to Cosmic Cantina is open until "),
        "{reply}"
    );
    assert!(
        text(reply).ends_with(". Reply with your order or tap the button."),
        "{reply}"
    );
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn an_unsure_trip_asks_and_only_the_announcers_yes_opens_it(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    scripted_gemini(&app, "get_profile", json!({}));

    // Gemini says Cosmic Cantina, but the words don't name it: ask.
    let trip = group_message(
        MAYA_TG,
        Some(MAYA_USERNAME),
        "Maya",
        "heading to the burrito place on Franklin at 6, anyone want anything?",
    );
    inbound(&app, &trip).await;
    let sent = app.sidecar.wait_for(1).await;
    // "at 6" closes at 6:10, unless 6:00 has just passed (then it leaves now).
    assert!(
        text(&sent[0].body).starts_with("Open a Ferry to Cosmic Cantina? Orders close "),
        "{}",
        sent[0].body
    );
    let yes = button_data(&sent[0].body, "Yes");
    let no = button_data(&sent[0].body, "No");

    inbound(
        &app,
        &press(
            GROUP_ID,
            true,
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            &yes,
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(2).await;
    assert_eq!(
        text(&sent[1].body),
        "Only the person heading out can answer that."
    );
    inbound(
        &app,
        &press(GROUP_ID, true, MAYA_TG, Some(MAYA_USERNAME), "Maya", &yes),
    )
    .await;
    let sent = app.sidecar.wait_for(3).await;
    assert!(text(&sent[2].body).starts_with("Ferry to Cosmic Cantina is open until "));
    assert_eq!(
        buttons(&sent[2].body)[0]["url"],
        "https://ferry.example/r/run_demo"
    );
    inbound(
        &app,
        &press(GROUP_ID, true, MAYA_TG, Some(MAYA_USERNAME), "Maya", &no),
    )
    .await;
    app.sidecar.assert_no_more_than(3).await;
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn the_judges_text_order_is_confirmed_then_placed(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    scripted_gemini(&app, "get_profile", json!({}));
    mayas_trip(&app, 0).await;

    let order = group_message(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "chicken burrito no beans",
    );
    assert!(inbound(&app, &order).await.is_empty());
    let sent = app.sidecar.wait_for(2).await;
    app.sidecar.assert_no_more_than(2).await;
    let draft = &sent[1].body;
    assert_eq!(draft["space_id"], GROUP_ID);
    assert_eq!(text(draft), JUDGE_DRAFT);
    let place = button_data(draft, "Place order");
    assert!(button_data(draft, "Cancel").ends_with(":cancel"));
    // Nothing is held before Place order.
    let orders: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.orders WHERE run_id = 'run_demo'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(orders, 0);

    // Only the judge's press places it.
    inbound(
        &app,
        &press(GROUP_ID, true, MAYA_TG, Some(MAYA_USERNAME), "Maya", &place),
    )
    .await;
    let sent = app.sidecar.wait_for(3).await;
    assert_eq!(
        text(&sent[2].body),
        "Only the person who drafted that order can place it."
    );
    inbound(
        &app,
        &press(
            GROUP_ID,
            true,
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            &place,
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(4).await;
    let placed = &sent[3].body;
    assert_eq!(
        text(placed),
        "Order placed: $13.16 is held from your Ferry balance. Maya brings it to Fetzer Gym (HackNC)."
    );
    let (order_id, customer, status, total, choices): (String, String, String, i64, Vec<String>) =
        sqlx::query_as(
            "SELECT o.id, o.customer_id, o.status, o.total_cents, l.choice_ids
               FROM ferry.orders o JOIN ferry.order_lines l ON l.order_id = o.id
              WHERE o.run_id = 'run_demo'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        (customer.as_str(), status.as_str(), total),
        ("usr_judge", "funded", 1316)
    );
    assert_eq!(choices, ["cho_cosmic_chicken_burrito_no_beans"]);
    assert_eq!(
        buttons(placed)[0]["url"],
        format!("https://ferry.example/o/{order_id}")
    );
    let held: i64 = sqlx::query_scalar(
        "SELECT amount_cents FROM ferry.ledger_entries WHERE idempotency_key = $1",
    )
    .bind(format!("order_hold:{order_id}"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(held, 1316);

    // A second press places nothing.
    inbound(
        &app,
        &press(
            GROUP_ID,
            true,
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            &place,
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(5).await;
    assert_eq!(text(&sent[4].body), "That order is placed already.");
    let orders: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.orders WHERE run_id = 'run_demo'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(orders, 1);
    assert_eq!(
        app.gemini.request_count(),
        1,
        "only the trip's classification"
    );
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn a_pending_draft_takes_edits_and_ambiguous_words_ask(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    let say = |text: &str| group_message(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", text);

    inbound(&app, &say("burrito no beans")).await;
    let sent = app.sidecar.wait_for(1).await;
    assert!(
        text(&sent[0].body).contains("Which one: "),
        "{}",
        sent[0].body
    );

    inbound(&app, &say("chicken burrito no beans")).await;
    let sent = app.sidecar.wait_for(2).await;
    assert_eq!(text(&sent[1].body), JUDGE_DRAFT);

    // $8.99 + $1.50 guac = $10.49; tax 79 c; total $14.78.
    inbound(&app, &say("add guac")).await;
    let sent = app.sidecar.wait_for(3).await;
    assert_eq!(
        text(&sent[2].body),
        "Jordan: Chicken Burrito (No beans, Guacamole) from Cosmic Cantina: $10.49 + tax $0.79 + Ferry fee $1.50 + tip $2.00 = $14.78, to Fetzer Gym (HackNC). Place the order?"
    );
    let drafts: Vec<String> =
        sqlx::query_scalar("SELECT status FROM ferry.order_drafts ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        drafts,
        ["cancelled", "pending"],
        "the edit replaces the draft"
    );
    // The old draft's button does nothing now.
    let old_place = button_data(&sent[1].body, "Place order");
    inbound(
        &app,
        &press(
            GROUP_ID,
            true,
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            &old_place,
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(4).await;
    assert_eq!(
        text(&sent[3].body),
        "That draft was cancelled; send your order again."
    );
    // Cancel on the new one drops it.
    let cancel = button_data(&sent[2].body, "Cancel");
    inbound(
        &app,
        &press(
            GROUP_ID,
            true,
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            &cancel,
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(5).await;
    assert_eq!(
        text(&sent[4].body),
        "OK, I dropped that order. Nothing was charged."
    );
    let orders: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.orders WHERE NOT is_demo_history")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(orders, 0);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_short_balance_keeps_the_draft_to_place_again(pool: PgPool) {
    seed_demo(&pool).await;
    map_demo_group(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    let order = group_message(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "chicken burrito no beans",
    );
    inbound(&app, &order).await;
    let sent = app.sidecar.wait_for(1).await;
    let place = button_data(&sent[0].body, "Place order");
    inbound(
        &app,
        &press(
            GROUP_ID,
            true,
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            &place,
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(2).await;
    assert_eq!(
        text(&sent[1].body),
        "Your Ferry balance is short for this order. Top up in the app, then tap Place order again."
    );
    let status: String = sqlx::query_scalar("SELECT status FROM ferry.order_drafts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "pending");

    ferry_core::payments::seed_demo_balances(&pool)
        .await
        .unwrap();
    inbound(
        &app,
        &press(
            GROUP_ID,
            true,
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            &place,
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(3).await;
    assert!(
        text(&sent[2].body).starts_with("Order placed: $13.16"),
        "{}",
        sent[2].body
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn the_runs_state_changes_are_announced_once_in_the_group(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    scripted_gemini(&app, "get_profile", json!({}));
    mayas_trip(&app, 0).await;
    let order = group_message(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "chicken burrito no beans",
    );
    inbound(&app, &order).await;
    let sent = app.sidecar.wait_for(2).await;
    let place = button_data(&sent[1].body, "Place order");
    inbound(
        &app,
        &press(
            GROUP_ID,
            true,
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            &place,
        ),
    )
    .await;
    app.sidecar.wait_for(3).await;

    // Maya closes the window in the app.
    runs::close_run(&app.state, "usr_maya", "run_demo")
        .await
        .unwrap();
    let sent = wait_long(&app, 4, 8).await;
    assert_eq!(sent[3].body["space_id"], GROUP_ID);
    assert_eq!(
        text(&sent[3].body),
        "Orders for the Ferry to Cosmic Cantina are closed: 1 order."
    );
    // Then she pays.
    runs::mark_purchased(
        &app.state,
        "usr_maya",
        "run_demo",
        &MarkPurchasedRequest {
            receipt_total_cents: 966,
            receipt_path: None,
        },
    )
    .await
    .unwrap();
    let sent = wait_long(&app, 5, 8).await;
    assert_eq!(text(&sent[4].body), "Maya paid at Cosmic Cantina.");
    // Each change once: nothing more arrives.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(app.sidecar.sent().len(), 5);
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn ordering_in_a_dm_through_geminis_tools(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    scripted_gemini(
        &app,
        "draft_order_from_text",
        json!({"text": "chicken burrito no beans"}),
    );
    let ask = dm(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "can I get a chicken burrito no beans from Maya's run?",
    );
    inbound(&app, &ask).await;
    let sent = app.sidecar.wait_for(1).await;
    let draft = &sent[0].body;
    assert_eq!(draft["space_id"], JUDGE_TG);
    // In a DM the line has no name in front.
    assert_eq!(text(draft), JUDGE_DRAFT.trim_start_matches("Jordan: "));
    let place = button_data(draft, "Place order");
    assert!(place.starts_with("order:"));
    let results = app.gemini.function_responses(1);
    assert_eq!(results[0].0, "draft_order_from_text");
    assert_eq!(results[0].1["total"], "$13.16");

    // "yes place it": Gemini calls confirm_order, which only shows the draft again with its
    // buttons. Nothing is placed until the judge taps Place order.
    scripted_gemini(&app, "confirm_order", json!({}));
    inbound(
        &app,
        &dm(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", "yes place it"),
    )
    .await;
    let sent = app.sidecar.wait_for(2).await;
    assert_eq!(
        text(&sent[1].body),
        JUDGE_DRAFT.trim_start_matches("Jordan: ")
    );
    assert_eq!(button_data(&sent[1].body, "Place order"), place);
    let orders: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.orders WHERE NOT is_demo_history")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(orders, 0, "words never place an order");
    inbound(
        &app,
        &press(
            JUDGE_TG,
            false,
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            &place,
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(3).await;
    assert_eq!(
        text(&sent[2].body),
        "Order placed: $13.16 is held from your Ferry balance. Maya brings it to Fetzer Gym (HackNC)."
    );
    // Both turns went through Gemini all the way: no privacy refusal, no fallback.
    for _ in 0..200 {
        let pending: i64 =
            sqlx::query_scalar("SELECT count(*) FROM ferry.agent_turns WHERE latency_ms IS NULL")
                .fetch_one(&pool)
                .await
                .unwrap();
        if pending == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let errors: Vec<Option<String>> =
        sqlx::query_scalar("SELECT error FROM ferry.agent_turns ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(errors, [None, None, None]);
    let results = app.gemini.function_responses(3);
    assert_eq!(results[0].0, "confirm_order");
    assert_eq!(results[0].1["placed"], false);
    // Gemini sees the drop-off as a placeholder, never its label.
    assert!(
        results[0].1["say"]
            .as_str()
            .unwrap()
            .ends_with("= $13.16, to {dropoff}. Place the order?"),
        "{}",
        results[0].1
    );
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn without_gemini_the_fallback_drafts_reports_and_cancels(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    let judge = |text: &str| dm(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", text);

    inbound(&app, &judge("chicken burrito no beans")).await;
    let sent = app.sidecar.wait_for(1).await;
    assert_eq!(
        text(&sent[0].body),
        JUDGE_DRAFT.trim_start_matches("Jordan: ")
    );
    // "yes place it" shows the draft again with its buttons; only the press places it.
    inbound(&app, &judge("yes place it")).await;
    let sent = app.sidecar.wait_for(2).await;
    assert_eq!(
        text(&sent[1].body),
        JUDGE_DRAFT.trim_start_matches("Jordan: ")
    );
    let place = button_data(&sent[1].body, "Place order");
    assert_eq!(place, button_data(&sent[0].body, "Place order"));
    inbound(
        &app,
        &press(
            JUDGE_TG,
            false,
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            &place,
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(3).await;
    assert!(
        text(&sent[2].body).starts_with("Order placed: $13.16"),
        "{}",
        sent[2].body
    );

    inbound(&app, &judge("what's the status of my order?")).await;
    let sent = app.sidecar.wait_for(4).await;
    assert_eq!(
        text(&sent[3].body),
        "Your Cosmic Cantina order is paid and waiting for pickup."
    );

    // "cancel my order" shows the order with its own Cancel button; only the press cancels it.
    inbound(&app, &judge("cancel my order please")).await;
    let sent = app.sidecar.wait_for(5).await;
    assert_eq!(
        text(&sent[4].body),
        "Tap the button to cancel your Cosmic Cantina order ($13.16). Nothing is cancelled until you do, and a cancelled order comes back to your Ferry balance in full."
    );
    let cancel = button_data(&sent[4].body, "Cancel Cosmic Cantina order");
    assert!(cancel.starts_with("cancel:ord_"), "{cancel}");
    let status = || async {
        sqlx::query_scalar::<_, String>("SELECT status FROM ferry.orders WHERE NOT is_demo_history")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    assert_eq!(status().await, "funded");
    // Someone else's press of it cancels nothing.
    inbound(
        &app,
        &press(
            DANA_TG,
            false,
            DANA_TG,
            Some(DANA_USERNAME),
            "Dana",
            &cancel,
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(6).await;
    assert_eq!(
        text(&sent[5].body),
        "Only the person who placed that order can cancel it."
    );
    assert_eq!(status().await, "funded");
    inbound(
        &app,
        &press(
            JUDGE_TG,
            false,
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            &cancel,
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(7).await;
    assert_eq!(
        text(&sent[6].body),
        "Cancelled your Cosmic Cantina order: $13.16 is back in your Ferry balance."
    );
    assert_eq!(status().await, "cancelled");
    // A second press finds it cancelled.
    inbound(
        &app,
        &press(
            JUDGE_TG,
            false,
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            &cancel,
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(8).await;
    assert_eq!(
        text(&sent[7].body),
        "That Cosmic Cantina order is cancelled, so it can't be cancelled now."
    );

    inbound(&app, &judge("is anyone going anywhere?")).await;
    let sent = app.sidecar.wait_for(9).await;
    assert!(
        text(&sent[8].body)
            .starts_with("Taking orders now: Cosmic Cantina with Maya, orders close "),
        "{}",
        sent[8].body
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn only_members_order_and_strangers_are_told_once(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    // Maya runs run_demo: her own "chicken burrito" in the group is chatter, not an order.
    inbound(
        &app,
        &group_message(
            MAYA_TG,
            Some(MAYA_USERNAME),
            "Maya",
            "chicken burrito no beans",
        ),
    )
    .await;
    app.sidecar.assert_no_more_than(0).await;
    // A stranger ordering is told to sign in, once.
    for _ in 0..2 {
        let order = group_message(STRANGER_TG, None, "Sam", "chicken burrito no beans");
        inbound(&app, &order).await;
    }
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert!(
        text(&sent[0].body).starts_with(UNKNOWN_SENDER),
        "{}",
        sent[0].body
    );
    // Dana (a member) orders fine.
    let dana = group_message(
        DANA_TG,
        Some(DANA_USERNAME),
        "Dana",
        "a quesadilla with steak",
    );
    inbound(&app, &dana).await;
    let sent = app.sidecar.wait_for(2).await;
    assert!(
        text(&sent[1].body)
            .starts_with("Dana: Quesadilla (Steak) from Cosmic Cantina: $9.99 + tax "),
        "{}",
        sent[1].body
    );
}
